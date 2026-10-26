//! vfcd — fan control daemon. Owns all EC writes: keeps the configured fan
//! tables in place (the firmware rewrites them on every profile switch),
//! applies a temperature guard, and serves the GUI/CLI over a Unix socket.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use vfc::proto::{Control, FanStatus, Guard, Request, Response, SOCKET, Status};
use vfc::tables::{self, FanMode, FanTable, MAX_RPM};
use vfc::{acpi_call, ec, sys};

const CONFIG: &str = "/etc/vfc/vfcd.json";
const TICK: Duration = Duration::from_secs(1);
/// Re-check enforced rows every N ticks (the firmware may have rewritten them).
const VERIFY_EVERY: u32 = 3;
/// Refresh the full table block for the GUI every N ticks.
const REFRESH_EVERY: u32 = 10;
const MAX_EVENTS: usize = 100;

static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    boot: Control,
    guard: Guard,
    /// Group allowed to use the socket.
    group: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { boot: Control::default(), guard: Guard::default(), group: "sudo".into() }
    }
}

impl Config {
    fn load() -> Self {
        match fs::read_to_string(CONFIG) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("vfcd: ignoring bad {CONFIG}: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(Path::new(CONFIG).parent().unwrap())?;
        fs::write(CONFIG, serde_json::to_string_pretty(self)? + "\n").with_context(|| format!("writing {CONFIG}"))
    }
}

struct Shared {
    config: Config,
    control: Control,
    status: Status,
    guard_active: bool,
    /// Apply now instead of waiting for the next verify.
    dirty: bool,
    /// Rewrite the firmware tables on the next tick (Restore request).
    restore: bool,
    events: VecDeque<(u64, String)>,
}

impl Shared {
    fn event(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        eprintln!("vfcd: {msg}");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back((now, msg));
    }
}

struct Daemon {
    shared: Mutex<Shared>,
    wake: Condvar,
}

impl Daemon {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn poke(&self) {
        self.lock().dirty = true;
        self.wake.notify_all();
    }
}

/// The control loop's own memory of what it wrote.
#[derive(Default)]
struct Loop {
    /// Table last written per fan (None = firmware owns the rows).
    applied: [Option<FanTable>; 2],
    ticks: u32,
    read_ms: f32,
}

fn guard_mode(fan: usize) -> FanMode {
    FanMode::Fixed { rpm: MAX_RPM[fan] }
}

impl Loop {
    fn sensors(&mut self, st: &mut Status) -> Result<()> {
        let t0 = Instant::now();
        for (fan, f) in st.fans.iter_mut().enumerate() {
            f.tach = ec::tach(fan)?;
            f.rpm = tables::tach_to_rpm(f.tach);
            f.level = ec::rram(ec::LEVEL[fan])?;
            f.temp = ec::rram(ec::CTL_TEMP[fan])?;
        }
        st.ec_mode = ec::rram(ec::MODE)?;
        let ms = t0.elapsed().as_secs_f32() * 1000.0 / 9.0;
        self.read_ms = if self.read_ms == 0.0 { ms } else { 0.9 * self.read_ms + 0.1 * ms };
        st.ec_read_ms = self.read_ms;

        st.cpu_temp = sys::cpu_temp();
        st.profile = sys::platform_profile();
        st.ac = sys::ac_online();
        st.battery = sys::battery();
        st.dgpu = sys::dgpu_state();
        Ok(())
    }

    /// Make the EC rows match `want`. Returns true if the firmware had
    /// changed rows we had already written.
    fn enforce(&mut self, fan: usize, want: &FanTable, force: bool) -> Result<bool> {
        let enforced = want.enforced(fan);
        let mut stale = Vec::new();
        for &(addr, v) in &enforced {
            if ec::rram(addr)? != v {
                stale.push((addr, v));
            }
        }
        let reset = !stale.is_empty() && self.applied[fan].as_ref() == Some(want);
        let new = self.applied[fan].as_ref() != Some(want);
        if !stale.is_empty() || new || force {
            // Duty first: it's where the EC starts once the target changes.
            for (addr, v) in want.duty_bytes(fan) {
                ec::wram(addr, v)?;
            }
            for (addr, v) in stale {
                ec::wram(addr, v)?;
            }
        }
        self.applied[fan] = Some(want.clone());
        Ok(reset)
    }

    fn tick(&mut self, d: &Daemon) -> Result<()> {
        let mut st = d.lock().status.clone();
        self.sensors(&mut st)?;

        let mut s = d.lock();
        let hot = st.fans.iter().map(|f| f32::from(f.temp)).fold(st.cpu_temp.unwrap_or(0.0), f32::max);
        let guard = s.config.guard.clone();
        let owned = s.control.fans.iter().any(|m| *m != FanMode::Stock);
        if guard.enabled && owned && !s.guard_active && hot >= f32::from(guard.trip) {
            s.guard_active = true;
            s.event(format!("guard: {hot:.0} °C >= {} °C, custom fans to full speed", guard.trip));
        } else if s.guard_active && (!guard.enabled || !owned || hot < f32::from(guard.release)) {
            s.guard_active = false;
            s.event(format!("guard released at {hot:.0} °C"));
            for fan in 0..2 {
                if s.control.fans[fan] == FanMode::Manual {
                    s.control.fans[fan] = FanMode::Stock;
                    s.event(format!("fan{fan}: manual tables were overwritten by the guard, back to stock"));
                }
            }
        }
        let modes: [FanMode; 2] = std::array::from_fn(|fan| match &s.control.fans[fan] {
            FanMode::Stock => FanMode::Stock,
            _ if s.guard_active => guard_mode(fan),
            m => m.clone(),
        });
        let force = std::mem::take(&mut s.dirty);
        let restore = std::mem::take(&mut s.restore);
        drop(s);

        // A fan handed back to the firmware needs its stock rows rewritten,
        // which only a profile switch does (for both fans).
        let mut force = force;
        let released = restore || (0..2).any(|f| self.applied[f].is_some() && modes[f] == FanMode::Stock);
        if released {
            sys::restore_tables()?;
            // The switch rewrote both fans; a custom one gets rewritten in
            // full below without being reported as a firmware reset.
            self.applied = [None, None];
            force = true;
            d.lock().event("firmware tables restored");
        }
        for (fan, mode) in modes.iter().enumerate() {
            if *mode == FanMode::Manual {
                self.applied[fan] = None;
            }
        }

        let verify = force || self.ticks.is_multiple_of(VERIFY_EVERY);
        if verify {
            for (fan, mode) in modes.iter().enumerate() {
                if let Some(want) = mode.table(fan)
                    && self.enforce(fan, &want, force)? {
                        d.lock().event(format!(
                            "fan{fan}: firmware rewrote the tables (profile {}), re-applied",
                            st.profile
                        ));
                    }
            }
        }
        if force || self.ticks.is_multiple_of(REFRESH_EVERY) || st.tables.is_empty() {
            st.tables = ec::read_block(ec::TABLES.0, ec::TABLES.1)?;
        }
        for (fan, f) in st.fans.iter_mut().enumerate() {
            fill_targets(f, &st.tables, fan);
        }
        self.ticks = self.ticks.wrapping_add(1);

        let mut s = d.lock();
        st.control = s.control.clone();
        st.boot = s.config.boot.clone();
        st.guard = s.config.guard.clone();
        st.guard_active = s.guard_active;
        st.profiles = sys::profile_choices();
        st.events = s.events.iter().cloned().collect();
        s.status = st;
        Ok(())
    }
}

fn fill_targets(f: &mut FanStatus, block: &[u8], fan: usize) {
    let t = FanTable::decode(block, fan);
    let i = usize::from(f.level).checked_sub(1);
    f.target_rpm = t.as_ref().and_then(|t| t.level_rpm(f.level));
    f.duty = t.zip(i).and_then(|(t, i)| t.duty.get(i).copied());
}

fn control_loop(d: Arc<Daemon>) {
    let mut lp = Loop::default();
    let mut errors = 0;
    loop {
        if STOP.load(Ordering::SeqCst) {
            let owned = lp.applied.iter().any(Option::is_some) || d.lock().control.fans.contains(&FanMode::Manual);
            if owned {
                match sys::restore_tables() {
                    Ok(()) => eprintln!("vfcd: stopping, firmware tables restored"),
                    Err(e) => eprintln!("vfcd: stopping, restore failed: {e:#}"),
                }
            }
            let _ = fs::remove_file(SOCKET);
            std::process::exit(0);
        }
        match lp.tick(&d) {
            Ok(()) => errors = 0,
            Err(e) => {
                errors += 1;
                // Don't flood the log with the same mailbox timeout.
                if errors <= 3 || errors % 60 == 0 {
                    d.lock().event(format!("error: {e:#}"));
                }
            }
        }
        let s = d.lock();
        if !s.dirty && !STOP.load(Ordering::SeqCst) {
            let _ = d.wake.wait_timeout(s, TICK);
        }
    }
}

fn handle(d: &Daemon, req: Request) -> Result<Response> {
    Ok(match req {
        Request::Status => Response::Status(Box::new(d.lock().status.clone())),
        Request::Tables => {
            let b = ec::read_block(ec::TABLES.0, ec::TABLES.1)?;
            d.lock().status.tables = b.clone();
            Response::Bytes(b)
        }
        Request::ReadEc { addr, len } => {
            if len > 0x1000 || u32::from(addr) + u32::from(len) > 0x10000 {
                bail!("range too large");
            }
            Response::Bytes(ec::scan(addr, len).0)
        }
        Request::WriteTable { addr, bytes } => {
            let end = addr as usize + bytes.len();
            if bytes.is_empty() || addr < ec::TABLES.0 || end > (ec::TABLES.0 + ec::TABLES.1) as usize {
                bail!("writes are limited to the table block 0x0A00-0x0AFF");
            }
            {
                let mut s = d.lock();
                for a in addr..end as u16 {
                    if let Some(fan) = tables::owner(a)
                        && s.control.fans[fan] != FanMode::Manual
                    {
                        s.control.fans[fan] = FanMode::Manual;
                        s.event(format!("fan{fan}: raw table edit, now manual"));
                    }
                }
            }
            for (i, v) in bytes.iter().enumerate() {
                ec::wram(addr + i as u16, *v)?;
            }
            d.lock().event(format!("wrote {} byte(s) at 0x{addr:04X}", bytes.len()));
            d.poke();
            Response::Ok
        }
        Request::SetControl(c) => {
            validate(&c)?;
            let mut s = d.lock();
            if s.control != c {
                s.event(format!("control: fan0 {}, fan1 {}", c.fans[0].label(), c.fans[1].label()));
                s.control = c;
            }
            drop(s);
            d.poke();
            Response::Ok
        }
        Request::SetBoot(c) => {
            validate(&c)?;
            let mut s = d.lock();
            s.config.boot = c;
            s.config.save()?;
            s.status.boot = s.config.boot.clone();
            s.event("boot control saved");
            Response::Ok
        }
        Request::SetGuard(g) => {
            if g.release >= g.trip || g.trip > 105 || g.trip < 60 {
                bail!("guard: trip must be 60-105 °C and release below trip");
            }
            let mut s = d.lock();
            s.config.guard = g;
            s.config.save()?;
            s.status.guard = s.config.guard.clone();
            s.event("guard settings saved");
            drop(s);
            d.poke();
            Response::Ok
        }
        Request::SetProfile(p) => {
            sys::set_platform_profile(&p)?;
            d.lock().event(format!("platform profile -> {p}"));
            d.poke();
            Response::Ok
        }
        Request::SetChargeLimit(pct) => {
            sys::set_charge_limit(pct)?;
            d.lock().event(format!("charge limit -> {pct}%"));
            Response::Ok
        }
        Request::Dgpu(action) => {
            let out = sys::dgpu_power(&action)?;
            d.lock().event(format!("dgpu {action}: {out}"));
            Response::Message(out)
        }
        Request::Restore => {
            let mut s = d.lock();
            s.control = Control::default();
            s.event("restore: both fans back to stock");
            // Only the control loop switches profiles, so two restores can't
            // interleave and leave the profile on the temporary one.
            s.restore = true;
            drop(s);
            d.poke();
            Response::Ok
        }
    })
}

fn validate(c: &Control) -> Result<()> {
    for (fan, m) in c.fans.iter().enumerate() {
        match m {
            FanMode::Curve { points, hysteresis } => {
                if points.is_empty() || points.len() > tables::LEVELS {
                    bail!("fan{fan}: a curve needs 1-{} points", tables::LEVELS);
                }
                if *hysteresis > 30 {
                    bail!("fan{fan}: hysteresis over 30 °C");
                }
                if points.iter().any(|p| p.temp > 110) {
                    bail!("fan{fan}: curve temperatures must be <= 110 °C");
                }
            }
            FanMode::Fixed { .. } | FanMode::Stock => {}
            FanMode::Manual => bail!("fan{fan}: manual is set by raw table writes, not directly"),
        }
    }
    Ok(())
}

fn serve(d: Arc<Daemon>, stream: UnixStream) {
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(&d, req).unwrap_or_else(|e| Response::Error(format!("{e:#}"))),
            Err(e) => Response::Error(format!("bad request: {e}")),
        };
        let mut json = serde_json::to_string(&resp).unwrap_or_else(|e| format!("{{\"Error\":\"{e}\"}}"));
        json.push('\n');
        if out.write_all(json.as_bytes()).is_err() {
            return;
        }
    }
}

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn group_id(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: getgrnam returns a pointer into static storage or null; we only
    // read gr_gid before any other call could overwrite it.
    let g = unsafe { libc::getgrnam(c.as_ptr()) };
    (!g.is_null()).then(|| unsafe { (*g).gr_gid })
}

fn main() -> Result<()> {
    if !acpi_call::available() {
        let _ = std::process::Command::new("modprobe").arg("acpi_call").status();
    }
    if !acpi_call::available() {
        bail!("acpi_call not available (modprobe acpi_call failed?)");
    }

    let config = Config::load();
    let d = Arc::new(Daemon {
        shared: Mutex::new(Shared {
            control: config.boot.clone(),
            config,
            status: Status::default(),
            guard_active: false,
            dirty: true,
            restore: false,
            events: VecDeque::new(),
        }),
        wake: Condvar::new(),
    });
    {
        let mut s = d.lock();
        let c = s.control.clone();
        s.event(format!("started: fan0 {}, fan1 {}", c.fans[0].label(), c.fans[1].label()));
    }

    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let _ = fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET).with_context(|| format!("binding {SOCKET}"))?;
    let group = d.lock().config.group.clone();
    let gid = group_id(&group).with_context(|| format!("no group {group:?}"))?;
    let path = std::ffi::CString::new(SOCKET)?;
    // SAFETY: valid C string; uid -1 leaves the owner unchanged.
    if unsafe { libc::chown(path.as_ptr(), u32::MAX, gid) } != 0 {
        bail!("chown {SOCKET}: {}", std::io::Error::last_os_error());
    }
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o660))?;

    {
        let d = d.clone();
        thread::spawn(move || control_loop(d));
    }
    for stream in listener.incoming().flatten() {
        let d = d.clone();
        thread::spawn(move || serve(d, stream));
    }
    Ok(())
}
