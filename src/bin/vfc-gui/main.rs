//! vfc-gui — desktop front end for vfcd.

mod backend;
mod presets;

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, RichText, Sense, Ui};
use egui_plot::{Legend, Line, LineStyle, MarkerShape, Plot, PlotPoints, Points, VLine};

use backend::Backend;
use presets::Presets;
use vfc::proto::{Control, Guard, Request, Status};
use vfc::tables::{self, CurvePoint, FanMode, FanTable, LEVELS, MAX_RPM, MIN_RPM, ROWS};

const FAN_COLOR: [Color32; 2] = [Color32::from_rgb(80, 160, 255), Color32::from_rgb(255, 150, 60)];
const CPU_COLOR: Color32 = Color32::from_rgb(235, 80, 80);
const WARN: Color32 = Color32::from_rgb(240, 190, 60);
const BAD: Color32 = Color32::from_rgb(235, 80, 80);
const GOOD: Color32 = Color32::from_rgb(110, 200, 120);
const HISTORY_SECS: f64 = 600.0;
const FAN_NAMES: [&str; 2] = ["Fan 0 (CPU side)", "Fan 1"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Vivobook Fan Control")
            .with_app_id("vfc-gui")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("vfc-gui", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Dashboard,
    Control,
    Tables,
    Explorer,
    System,
    Log,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Stock,
    Fixed,
    Curve,
    Manual,
}

/// Editable copy of one fan's mode.
#[derive(Clone)]
struct FanDraft {
    kind: Kind,
    rpm: u32,
    off: bool,
    points: Vec<CurvePoint>,
    hysteresis: u8,
    drag: Option<usize>,
}

impl FanDraft {
    fn from_mode(fan: usize, m: &FanMode) -> Self {
        let mut d = Self {
            kind: Kind::Stock,
            rpm: 1800.min(MAX_RPM[fan]),
            off: false,
            points: tables::default_curve(fan),
            hysteresis: 4,
            drag: None,
        };
        match m {
            FanMode::Stock => {}
            FanMode::Manual => d.kind = Kind::Manual,
            FanMode::Fixed { rpm } => {
                d.kind = Kind::Fixed;
                d.off = *rpm == 0;
                if *rpm != 0 {
                    d.rpm = *rpm;
                }
            }
            FanMode::Curve { points, hysteresis } => {
                d.kind = Kind::Curve;
                d.points = points.clone();
                d.hysteresis = *hysteresis;
            }
        }
        d
    }

    fn to_mode(&self) -> FanMode {
        match self.kind {
            Kind::Stock => FanMode::Stock,
            Kind::Manual => FanMode::Manual,
            Kind::Fixed => FanMode::Fixed { rpm: if self.off { 0 } else { self.rpm } },
            Kind::Curve => {
                let mut points = self.points.clone();
                points.sort_by_key(|p| p.temp);
                FanMode::Curve { points, hysteresis: self.hysteresis }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    t: f64,
    cpu: Option<f32>,
    temp: [u8; 2],
    rpm: [u32; 2],
    target: [Option<u32>; 2],
}

struct App {
    backend: Backend,
    tab: Tab,
    history: VecDeque<Sample>,
    last_sample: Option<Instant>,
    drafts: Option<[FanDraft; 2]>,
    /// Control the drafts were last synced from.
    synced: Option<Control>,
    presets: Presets,
    preset_name: String,
    preset_sel: usize,
    /// Table editor: working copy and the EC block it was based on.
    tbl_edit: Vec<u8>,
    tbl_base: Vec<u8>,
    tbl_gen: u64,
    ex_addr: u16,
    ex_len: u16,
    ex_auto: bool,
    ex_last: Option<Instant>,
    ex_prev: Option<(u16, Vec<u8>)>,
    ex_changed: Vec<Option<Instant>>,
    guard_edit: Option<Guard>,
    charge_edit: Option<u8>,
    confirm_full_scan: bool,
}

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        ctx.set_zoom_factor(1.1);
        Self {
            backend: Backend::start(ctx.clone()),
            tab: match std::env::args().nth(1).as_deref() {
                Some("control") => Tab::Control,
                Some("tables") => Tab::Tables,
                Some("explorer") => Tab::Explorer,
                Some("system") => Tab::System,
                Some("log") => Tab::Log,
                _ => Tab::Dashboard,
            },
            history: VecDeque::new(),
            last_sample: None,
            drafts: None,
            synced: None,
            presets: Presets::load(),
            preset_name: String::new(),
            preset_sel: 0,
            tbl_edit: Vec::new(),
            tbl_base: Vec::new(),
            tbl_gen: 0,
            ex_addr: 0x0380,
            ex_len: 0x100,
            ex_auto: false,
            ex_last: None,
            ex_prev: None,
            ex_changed: Vec::new(),
            guard_edit: None,
            charge_edit: None,
            confirm_full_scan: false,
        }
    }

    fn record(&mut self, st: &Status) {
        if self.last_sample.is_some_and(|t| t.elapsed() < Duration::from_millis(900)) {
            return;
        }
        self.last_sample = Some(Instant::now());
        self.history.push_back(Sample {
            t: now_secs(),
            cpu: st.cpu_temp,
            temp: [st.fans[0].temp, st.fans[1].temp],
            rpm: [st.fans[0].rpm, st.fans[1].rpm],
            target: [st.fans[0].target_rpm, st.fans[1].target_rpm],
        });
        let cutoff = now_secs() - HISTORY_SECS;
        while self.history.front().is_some_and(|s| s.t < cutoff) {
            self.history.pop_front();
        }
    }

    /// Keep drafts in step with the daemon unless the user is mid-edit.
    fn sync_drafts(&mut self, st: &Status) {
        let edited = match (&self.drafts, &self.synced) {
            (Some(d), Some(c)) => d.iter().map(FanDraft::to_mode).collect::<Vec<_>>() != c.fans.to_vec(),
            _ => false,
        };
        if self.synced.as_ref() != Some(&st.control) && !edited || self.drafts.is_none() {
            self.drafts = Some(std::array::from_fn(|f| FanDraft::from_mode(f, &st.control.fans[f])));
            self.synced = Some(st.control.clone());
        }
    }

    fn sync_tables(&mut self, st: &Status, generation: u64) {
        if generation == self.tbl_gen || st.tables.len() != 256 {
            return;
        }
        self.tbl_gen = generation;
        let untouched = self.tbl_edit == self.tbl_base;
        self.tbl_base = st.tables.clone();
        if untouched || self.tbl_edit.len() != 256 {
            self.tbl_edit = st.tables.clone();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let (status, connected, error, busy, generation) = {
            let s = self.backend.state();
            (s.status.clone(), s.connected, s.error.clone(), s.busy, s.tables_gen)
        };
        if let Some(st) = &status {
            if connected {
                self.record(st);
            }
            self.sync_drafts(st);
            self.sync_tables(st, generation);
        }
        if self.ex_auto && self.ex_last.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) && busy == 0 {
            self.ex_last = Some(Instant::now());
            self.backend.read(Request::ReadEc { addr: self.ex_addr, len: self.ex_len });
        }

        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui, status.as_ref(), connected));
        egui::Panel::bottom("notices").show(ui, |ui| self.notices(ui, busy));
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(st) = status.filter(|_| connected) else {
                disconnected(ui, error.as_deref());
                return;
            };
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Dashboard => self.dashboard(ui, &st),
                Tab::Control => self.control(ui, &st),
                Tab::Tables => self.tables(ui, &st),
                Tab::Explorer => self.explorer(ui),
                Tab::System => self.system(ui, &st),
                Tab::Log => log(ui, &st),
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

fn disconnected(ui: &mut Ui, error: Option<&str>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.heading("Not connected to vfcd");
        ui.add_space(8.0);
        ui.label("The fan daemon owns all EC access. Start it with:");
        ui.code("sudo systemctl enable --now vfcd");
        if let Some(e) = error {
            ui.add_space(8.0);
            ui.colored_label(BAD, e);
        }
        ui.add_space(12.0);
        if ui.button("Start vfcd (asks for your password)").clicked() {
            let _ = std::process::Command::new("pkexec").args(["systemctl", "start", "vfcd"]).spawn();
        }
    });
}

fn temp_color(t: f32) -> Color32 {
    if t >= 90.0 {
        BAD
    } else if t >= 75.0 {
        WARN
    } else {
        GOOD
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut Ui, st: Option<&Status>, connected: bool) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (tab, name) in [
                (Tab::Dashboard, "Dashboard"),
                (Tab::Control, "Fan control"),
                (Tab::Tables, "EC tables"),
                (Tab::Explorer, "EC explorer"),
                (Tab::System, "System"),
                (Tab::Log, "Log"),
            ] {
                ui.selectable_value(&mut self.tab, tab, RichText::new(name).size(15.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let restore = ui
                    .add_enabled(connected, egui::Button::new(RichText::new("⟲ Stock").strong()))
                    .on_hover_text("Hand both fans back to the firmware (rewrites the stock tables)");
                if restore.clicked() {
                    self.backend.send(Request::Restore, "restore stock");
                }
                let Some(st) = st.filter(|_| connected) else {
                    ui.colored_label(BAD, "● disconnected");
                    return;
                };
                if st.guard_active {
                    ui.colored_label(BAD, RichText::new("GUARD: full speed").strong());
                }
                ui.separator();
                for fan in (0..2).rev() {
                    ui.colored_label(FAN_COLOR[fan], format!("fan{fan} {} rpm", st.fans[fan].rpm));
                }
                if let Some(t) = st.cpu_temp {
                    ui.colored_label(temp_color(t), RichText::new(format!("CPU {t:.0} °C")).strong());
                }
                ui.label(format!("profile: {}", st.profile));
            });
        });
        ui.add_space(2.0);
    }

    fn notices(&mut self, ui: &mut Ui, busy: usize) {
        let s = self.backend.state();
        let recent: Vec<_> = s
            .notices
            .iter()
            .filter(|n| n.at.elapsed() < Duration::from_secs(if n.ok { 5 } else { 15 }))
            .map(|n| (n.ok, n.text.clone()))
            .collect();
        drop(s);
        ui.horizontal(|ui| {
            if busy > 0 {
                ui.spinner();
            }
            if recent.is_empty() {
                ui.weak("ready");
            }
            for (ok, text) in recent.iter().rev().take(2) {
                ui.colored_label(if *ok { GOOD } else { BAD }, text);
            }
        });
    }

    // ---------------------------------------------------------------- dashboard

    fn dashboard(&mut self, ui: &mut Ui, st: &Status) {
        ui.columns(3, |cols| {
            for fan in 0..2 {
                fan_card(&mut cols[fan], st, fan);
            }
            let ui = &mut cols[2];
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new("Temperatures").strong());
                if let Some(t) = st.cpu_temp {
                    ui.label(RichText::new(format!("{t:.0} °C")).size(34.0).color(temp_color(t)));
                    ui.weak("CPU package");
                }
                ui.add_space(4.0);
                ui.label(format!("EC control temp: fan0 {} °C, fan1 {} °C", st.fans[0].temp, st.fans[1].temp));
                ui.label(format!("Platform profile: {} (EC mode {})", st.profile, st.ec_mode));
                let g = &st.guard;
                if g.enabled {
                    ui.label(format!("Guard: full speed at {} °C, until < {} °C", g.trip, g.release));
                } else {
                    ui.colored_label(WARN, "Guard disabled");
                }
                ui.label(format!(
                    "Power: {}  ·  battery {}% {}",
                    if st.ac { "AC" } else { "battery" },
                    st.battery.capacity.map_or("?".into(), |c| c.to_string()),
                    st.battery.status
                ));
            });
        });

        ui.add_space(8.0);
        let link = egui::Id::unique("history");
        let now = now_secs();
        let series = |f: &dyn Fn(&Sample) -> Option<f64>| -> PlotPoints<'static> {
            self.history.iter().filter_map(|s| Some([s.t - now, f(s)?])).collect()
        };
        ui.label(RichText::new("Temperature (last 10 min)").strong());
        Plot::new("temps")
            .height(200.0)
            .legend(Legend::default())
            .link_axis(link, [true, false])
            .link_cursor(link, [true, false])
            .include_y(30.0)
            .include_y(100.0)
            .default_x_bounds(-HISTORY_SECS, 0.0)
            .x_axis_formatter(|m, _| format!("{:.0}s", m.value))
            .y_axis_formatter(|m, _| format!("{:.0}°", m.value))
            .show(ui, |p| {
                p.line(Line::new("CPU", series(&|s| s.cpu.map(f64::from))).color(CPU_COLOR));
                for fan in 0..2 {
                    p.line(
                        Line::new(format!("EC fan{fan}"), series(&|s| Some(f64::from(s.temp[fan]))))
                            .color(FAN_COLOR[fan])
                            .style(LineStyle::dashed_dense()),
                    );
                }
            });
        ui.label(RichText::new("Fan speed").strong());
        Plot::new("rpms")
            .height(220.0)
            .legend(Legend::default())
            .link_axis(link, [true, false])
            .link_cursor(link, [true, false])
            .include_y(0.0)
            .include_y(3000.0)
            .default_x_bounds(-HISTORY_SECS, 0.0)
            .x_axis_formatter(|m, _| format!("{:.0}s", m.value))
            .show(ui, |p| {
                for fan in 0..2 {
                    p.line(Line::new(format!("fan{fan}"), series(&|s| Some(f64::from(s.rpm[fan])))).color(FAN_COLOR[fan]));
                    p.line(
                        Line::new(format!("fan{fan} target"), series(&|s| s.target[fan].map(f64::from)))
                            .color(FAN_COLOR[fan].gamma_multiply(0.6))
                            .style(LineStyle::dashed_loose()),
                    );
                }
            });
    }

    // ---------------------------------------------------------------- control

    fn control(&mut self, ui: &mut Ui, st: &Status) {
        let Some(mut drafts) = self.drafts.take() else { return };
        let applied: Vec<FanMode> = drafts.iter().map(FanDraft::to_mode).collect();
        let dirty = applied != st.control.fans.to_vec();

        ui.horizontal(|ui| {
            let can_apply = !drafts.iter().any(|d| d.kind == Kind::Manual);
            if ui
                .add_enabled(dirty && can_apply, egui::Button::new(RichText::new("Apply").strong().size(16.0)))
                .clicked()
            {
                let c = Control { fans: [drafts[0].to_mode(), drafts[1].to_mode()] };
                self.synced = None;
                self.backend.send(Request::SetControl(c), "apply");
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                drafts = std::array::from_fn(|f| FanDraft::from_mode(f, &st.control.fans[f]));
            }
            if dirty {
                ui.colored_label(WARN, "unapplied changes");
            }
            ui.separator();
            let boot_same = st.boot == st.control;
            if ui
                .add_enabled(!boot_same && !st.control.fans.contains(&FanMode::Manual), egui::Button::new("Use at boot"))
                .on_hover_text("vfcd applies this control every time it starts")
                .clicked()
            {
                self.backend.send(Request::SetBoot(st.control.clone()), "save boot control");
            }
            ui.weak(format!(
                "boot: fan0 {}, fan1 {}",
                st.boot.fans[0].label(),
                st.boot.fans[1].label()
            ));
        });

        ui.horizontal(|ui| {
            ui.label("Preset:");
            let names = self.presets.names();
            self.preset_sel = self.preset_sel.min(names.len().saturating_sub(1));
            egui::ComboBox::from_id_salt("preset")
                .selected_text(names.get(self.preset_sel).cloned().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.preset_sel, i, n);
                    }
                });
            if ui.button("Load").on_hover_text("Load into the editor (then Apply)").clicked()
                && let Some(c) = self.presets.get(self.preset_sel)
            {
                drafts = std::array::from_fn(|f| FanDraft::from_mode(f, &c.fans[f]));
            }
            if self.presets.is_user(self.preset_sel) && ui.button("Delete").clicked() {
                self.presets.delete(self.preset_sel);
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("name").desired_width(140.0));
            let can_save = !self.preset_name.trim().is_empty() && !drafts.iter().any(|d| d.kind == Kind::Manual);
            if ui.add_enabled(can_save, egui::Button::new("Save editor as preset")).clicked() {
                let c = Control { fans: [drafts[0].to_mode(), drafts[1].to_mode()] };
                self.presets.save(self.preset_name.trim(), c);
                self.preset_name.clear();
            }
        });
        ui.separator();

        ui.columns(2, |cols| {
            for fan in 0..2 {
                fan_editor(&mut cols[fan], fan, &mut drafts[fan], st);
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Copy fan0 → fan1").clicked() {
                drafts[1] = copy_draft(&drafts[0], 1);
            }
            if ui.button("Copy fan1 → fan0").clicked() {
                drafts[0] = copy_draft(&drafts[1], 0);
            }
        });
        ui.add_space(6.0);
        ui.weak(
            "Curve: each point is a level. The fan runs at a point's speed from its temperature up to the next point's. \
             Below the first point the fan is at level 0 and stops — put the first point at 0 °C for a minimum speed \
             (stored as 20 °C). \
             Drag points; double-click to add; right-click to remove. Hysteresis: a level drops only once the temperature \
             falls this far below its point, and the EC waits ~20-30 s before stepping down. From standstill a fan can take ~10 s to start.",
        );
        self.drafts = Some(drafts);
    }

    // ---------------------------------------------------------------- tables

    fn tables(&mut self, ui: &mut Ui, st: &Status) {
        if self.tbl_edit.len() != 256 {
            ui.label("No table read yet.");
            return;
        }
        let changed: Vec<usize> = (0..256).filter(|&i| self.tbl_edit[i] != self.tbl_base[i]).collect();
        ui.horizontal(|ui| {
            if ui.button("Reload from EC").clicked() {
                self.tbl_edit = self.tbl_base.clone();
                self.backend.read(Request::Tables);
            }
            if ui.add_enabled(!changed.is_empty(), egui::Button::new("Discard edits")).clicked() {
                self.tbl_edit = self.tbl_base.clone();
            }
            let write = ui.add_enabled(
                !changed.is_empty(),
                egui::Button::new(RichText::new(format!("Write {} byte(s)", changed.len())).strong()),
            );
            if write.clicked() {
                for (start, bytes) in runs(&changed, &self.tbl_edit) {
                    self.backend.send(
                        Request::WriteTable { addr: tables::TABLES_START + start as u16, bytes },
                        format!("write 0x{:04X}", tables::TABLES_START + start as u16),
                    );
                }
                self.backend.read(Request::Tables);
            }
            ui.weak("Writing switches the touched fan to manual: vfcd stops enforcing it (the guard still applies).");
        });
        ui.add_space(4.0);

        for fan in 0..2 {
            ui.add_space(6.0);
            let level = st.fans[fan].level;
            ui.label(
                RichText::new(format!("{} — level {level}, mode {}", FAN_NAMES[fan], st.control.fans[fan].label()))
                    .strong()
                    .color(FAN_COLOR[fan]),
            );
            table_grid(ui, fan, &mut self.tbl_edit, &self.tbl_base, level);
        }

        ui.add_space(10.0);
        ui.collapsing("Raw block 0x0A00–0x0AFF (hex, editable)", |ui| {
            hex_editor(ui, &mut self.tbl_edit, &self.tbl_base);
        });
    }

    // ---------------------------------------------------------------- explorer

    fn explorer(&mut self, ui: &mut Ui) {
        ui.label("Read-only view of EC RAM through the firmware's RRAM mailbox (~1.2 ms per byte).");
        ui.horizontal(|ui| {
            ui.label("start");
            ui.add(egui::DragValue::new(&mut self.ex_addr).hexadecimal(4, false, true).prefix("0x").range(0..=0xFFFF));
            ui.label("length");
            ui.add(egui::DragValue::new(&mut self.ex_len).range(16..=0x1000).hexadecimal(3, false, true).prefix("0x"));
            if ui.button("Read").clicked() {
                if self.ex_len > 0x400 && !self.confirm_full_scan {
                    self.confirm_full_scan = true;
                } else {
                    self.confirm_full_scan = false;
                    self.backend.read(Request::ReadEc { addr: self.ex_addr, len: self.ex_len });
                }
            }
            ui.checkbox(&mut self.ex_auto, "auto-refresh (2 s)");
        });
        if self.confirm_full_scan {
            ui.colored_label(WARN, format!("Reading {} bytes takes ~{:.1} s — press Read again to confirm.", self.ex_len, f32::from(self.ex_len) * 0.0012));
        }
        ui.horizontal_wrapped(|ui| {
            ui.weak("jump:");
            for (name, addr, len) in [
                ("fan registers", 0x0380u16, 0x0100u16),
                ("temps 0x0570", 0x0570, 0x0010),
                ("limits 0x05C0", 0x05C0, 0x0010),
                ("mode 0x08F0", 0x08F0, 0x0010),
                ("fan tables", 0x0A00, 0x0100),
                ("commit flags 0x0BE0", 0x0BE0, 0x0010),
                ("all 0x0000–0x0FFF", 0x0000, 0x1000),
            ] {
                if ui.small_button(name).clicked() {
                    self.ex_addr = addr;
                    self.ex_len = len;
                    self.ex_auto = false;
                    self.backend.read(Request::ReadEc { addr, len });
                }
            }
        });
        ui.separator();

        let read = self.backend.state().ec_read.clone();
        let Some((start, bytes)) = read else {
            ui.weak("Nothing read yet.");
            return;
        };
        // Track per-byte changes between successive reads of the same range.
        let same_range = self.ex_prev.as_ref().is_some_and(|(s, b)| *s == start && b.len() == bytes.len());
        if !same_range {
            self.ex_changed = vec![None; bytes.len()];
        } else if let Some((_, prev)) = &self.ex_prev
            && *prev != bytes
        {
            for (i, (a, b)) in prev.iter().zip(&bytes).enumerate() {
                if a != b {
                    self.ex_changed[i] = Some(Instant::now());
                }
            }
        }
        self.ex_prev = Some((start, bytes.clone()));

        egui::Grid::new("ex").striped(true).spacing([6.0, 2.0]).show(ui, |ui| {
            ui.label("");
            for c in 0..16 {
                ui.weak(format!("{c:X}"));
            }
            ui.end_row();
            for (r, row) in bytes.chunks(16).enumerate() {
                let base = start as usize + r * 16;
                ui.monospace(RichText::new(format!("{base:04X}")).weak());
                for (c, b) in row.iter().enumerate() {
                    let i = r * 16 + c;
                    let addr = base + c;
                    let recent = self.ex_changed.get(i).copied().flatten().is_some_and(|t| t.elapsed() < Duration::from_secs(6));
                    let mut text = RichText::new(format!("{b:02x}")).monospace();
                    if recent {
                        text = text.color(Color32::BLACK).background_color(WARN);
                    } else if *b == 0 {
                        text = text.weak();
                    }
                    let word = bytes.get(i + 1).map(|hi| u16::from_le_bytes([*b, *hi]));
                    ui.label(text).on_hover_text(format!(
                        "0x{addr:04X}\nu8 {b}  i8 {}\nu16 LE {}{}",
                        *b as i8,
                        word.map_or("-".into(), |w| w.to_string()),
                        known_name(addr as u16).map_or(String::new(), |n| format!("\n{n}"))
                    ));
                }
                ui.end_row();
            }
        });
    }

    // ---------------------------------------------------------------- system

    fn system(&mut self, ui: &mut Ui, st: &Status) {
        section(ui, "Platform profile", |ui| {
            ui.horizontal(|ui| {
                for p in &st.profiles {
                    if ui.selectable_label(st.profile == *p, p).clicked() && st.profile != *p {
                        self.backend.send(Request::SetProfile(p.clone()), format!("profile {p}"));
                    }
                }
            });
            ui.weak(
                "Also sets the CPU power limits. Switching makes the firmware rewrite the fan tables; \
                 vfcd re-applies custom fans within ~3 s.",
            );
        });

        section(ui, "Temperature guard", |ui| {
            let mut g = self.guard_edit.clone().unwrap_or_else(|| st.guard.clone());
            ui.checkbox(&mut g.enabled, "enabled");
            ui.add(egui::Slider::new(&mut g.trip, 70..=105).text("trip: custom fans to full speed at (°C)"));
            let max_release = g.trip.saturating_sub(5);
            g.release = g.release.min(max_release);
            ui.add(egui::Slider::new(&mut g.release, 50..=max_release).text("release below (°C)"));
            ui.weak("Uses the hottest of the CPU package and both EC control temperatures. Stock fans are never touched.");
            if !g.enabled {
                ui.colored_label(WARN, "Without the guard a low fixed speed under load relies on CPU throttling alone.");
            }
            let changed = g != st.guard;
            let (mut save, mut revert) = (false, false);
            ui.horizontal(|ui| {
                save = ui.add_enabled(changed, egui::Button::new("Save guard")).clicked();
                revert = changed && ui.button("Revert").clicked();
            });
            if save {
                self.backend.send(Request::SetGuard(g), "guard");
                self.guard_edit = None;
            } else if revert || !changed {
                self.guard_edit = None;
            } else {
                self.guard_edit = Some(g);
            }
        });

        section(ui, "Battery", |ui| {
            let b = &st.battery;
            ui.label(format!(
                "{}% · {} · {} · {}",
                b.capacity.map_or("?".into(), |c| c.to_string()),
                b.status,
                b.power.map_or("? W".into(), |p| format!("{p:.1} W")),
                if st.ac { "on AC" } else { "on battery" }
            ));
            if let Some(cur) = b.charge_limit {
                let mut v = self.charge_edit.unwrap_or(cur);
                let mut set = false;
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut v, 20..=100).suffix("%").text("charge limit"));
                    for preset in [60u8, 80, 100] {
                        if ui.small_button(format!("{preset}%")).clicked() {
                            v = preset;
                        }
                    }
                    set = ui.add_enabled(v != cur, egui::Button::new("Set")).clicked();
                });
                if set {
                    self.backend.send(Request::SetChargeLimit(v), format!("charge limit {v}%"));
                }
                self.charge_edit = (v != cur && !set).then_some(v);
            }
        });

        section(ui, "NVIDIA dGPU", |ui| {
            ui.label(format!("state: {}", st.dgpu));
            ui.horizontal(|ui| {
                for (a, tip) in [
                    ("auto", "Follow AC: on when plugged in, rail off on battery"),
                    ("on", "Rescan the PCI bus: GPU available (runtime PM still suspends it when idle)"),
                    ("off", "Remove the GPU from the bus so its power rail turns off"),
                ] {
                    if ui.button(a).on_hover_text(tip).clicked() {
                        self.backend.send(Request::Dgpu(a.into()), format!("dGPU {a}"));
                    }
                }
            });
            ui.weak("Uses /usr/local/sbin/dgpu-power; refuses to remove the GPU while a program uses it. HDMI needs it on.");
        });

        section(ui, "Daemon", |ui| {
            ui.label(format!("EC read: {:.2} ms/byte", st.ec_read_ms));
            ui.label(format!("boot control: fan0 {}, fan1 {}", st.boot.fans[0].label(), st.boot.fans[1].label()));
            ui.weak("Config: /etc/vfc/vfcd.json · socket /run/vfcd.sock (group sudo) · on stop vfcd restores the stock tables.");
        });
    }
}

// ---------------------------------------------------------------- widgets

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(RichText::new(title).strong().size(15.0));
        ui.add_space(2.0);
        body(ui);
    });
    ui.add_space(6.0);
}

fn fan_card(ui: &mut Ui, st: &Status, fan: usize) {
    let f = &st.fans[fan];
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(RichText::new(FAN_NAMES[fan]).strong().color(FAN_COLOR[fan]));
        ui.label(RichText::new(format!("{} rpm", f.rpm)).size(34.0));
        let max = MAX_RPM[fan] as f32;
        ui.add(egui::ProgressBar::new(f.rpm as f32 / max).fill(FAN_COLOR[fan].gamma_multiply(0.7)));
        ui.label(match f.target_rpm {
            Some(t) if t > 0 => format!("target {t} rpm"),
            Some(_) => "target: off".into(),
            None => "level 0: no table entry (fan stops)".into(),
        });
        ui.label(format!(
            "level {}/13 · duty {} · EC temp {} °C",
            f.level,
            f.duty.map_or("-".into(), |d| d.to_string()),
            f.temp
        ));
        let mode = &st.control.fans[fan];
        let text = if st.guard_active && *mode != FanMode::Stock {
            RichText::new("mode: GUARD (full speed)").color(BAD)
        } else {
            RichText::new(format!("mode: {}", mode.label()))
        };
        ui.label(text);
    });
}

fn copy_draft(from: &FanDraft, to: usize) -> FanDraft {
    let mut d = from.clone();
    d.rpm = tables::clamp_rpm(to, d.rpm).max(MIN_RPM);
    for p in &mut d.points {
        p.rpm = tables::clamp_rpm(to, p.rpm);
    }
    d.drag = None;
    d
}

fn fan_editor(ui: &mut Ui, fan: usize, d: &mut FanDraft, st: &Status) {
    ui.label(RichText::new(FAN_NAMES[fan]).strong().size(16.0).color(FAN_COLOR[fan]));
    ui.horizontal(|ui| {
        ui.selectable_value(&mut d.kind, Kind::Stock, "Stock").on_hover_text("Firmware curve of the platform profile");
        ui.selectable_value(&mut d.kind, Kind::Fixed, "Fixed speed").on_hover_text("One speed, whatever the temperature");
        ui.selectable_value(&mut d.kind, Kind::Curve, "Curve").on_hover_text("Your own temperature → speed curve");
        if d.kind == Kind::Manual {
            ui.colored_label(WARN, "Manual (raw table edits) — pick a mode to take over");
        }
    });
    ui.add_space(4.0);
    let f = &st.fans[fan];
    match d.kind {
        Kind::Stock => {
            ui.label(format!("Following the firmware's \"{}\" profile.", st.profile));
            if let Some(t) = FanTable::decode(&st.tables, fan)
                && st.control.fans[fan] == FanMode::Stock
            {
                let pts: Vec<CurvePoint> = (0..LEVELS)
                    .filter(|&i| t.up[i] < 255)
                    .map(|i| CurvePoint { temp: t.up[i], rpm: tables::tach_to_rpm(t.target[i]) })
                    .collect();
                curve_plot(ui, fan, &pts, None, f, false);
            }
        }
        Kind::Manual => {
            ui.label("vfcd is not enforcing this fan; edit it in the EC tables tab.");
        }
        Kind::Fixed => {
            ui.checkbox(&mut d.off, "Off (0 rpm)");
            ui.add_enabled_ui(!d.off, |ui| {
                ui.add(egui::Slider::new(&mut d.rpm, MIN_RPM..=MAX_RPM[fan]).step_by(50.0).suffix(" rpm"));
                ui.horizontal(|ui| {
                    for (name, pct) in [("min", 0u32), ("25%", 25), ("50%", 50), ("75%", 75), ("max", 100)] {
                        if ui.small_button(name).clicked() {
                            d.rpm = MIN_RPM + (MAX_RPM[fan] - MIN_RPM) * pct / 100;
                        }
                    }
                });
            });
            if d.off {
                ui.colored_label(WARN, "Fan off: cooling relies on the other fan, the guard and CPU throttling.");
                if fan == 1 {
                    ui.weak("Untested whether the EC fully stops fan1 at duty 0.");
                }
            } else if d.rpm < 1000 {
                ui.weak("Very low speeds may stall; the EC will keep trying to restart the fan.");
            }
            ui.label(format!(
                "EC tach target {} · start duty {}",
                tables::rpm_to_tach(if d.off { 0 } else { d.rpm }),
                if d.off { 0 } else { tables::duty_for(fan, d.rpm) }
            ));
        }
        Kind::Curve => {
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut d.hysteresis, 0..=20).suffix(" °C").text("hysteresis"));
                if ui.small_button("reset curve").clicked() {
                    d.points = tables::default_curve(fan);
                }
            });
            let mut pts = d.points.clone();
            let edit = curve_plot(ui, fan, &pts, d.drag, f, true);
            if let Some(e) = edit {
                match e {
                    CurveEdit::DragStart(i) => d.drag = Some(i),
                    CurveEdit::Drag(p) => {
                        if let Some(i) = d.drag
                            && let Some(pt) = pts.get_mut(i)
                        {
                            *pt = p;
                        }
                    }
                    CurveEdit::DragEnd => d.drag = None,
                    CurveEdit::Add(p) if pts.len() < LEVELS => pts.push(p),
                    CurveEdit::Remove(i) if pts.len() > 1 => {
                        pts.remove(i);
                    }
                    _ => {}
                }
                if d.drag.is_none() {
                    pts.sort_by_key(|p| p.temp);
                }
                d.points = pts;
            }
            points_table(ui, fan, d);
        }
    }
}

fn points_table(ui: &mut Ui, fan: usize, d: &mut FanDraft) {
    let mut remove = None;
    egui::Grid::new(("pts", fan)).striped(true).show(ui, |ui| {
        ui.weak("#");
        ui.weak("from °C");
        ui.weak("rpm");
        ui.weak("");
        ui.end_row();
        for (i, p) in d.points.iter_mut().enumerate() {
            ui.label(format!("L{}", i + 1));
            ui.add(egui::DragValue::new(&mut p.temp).range(0..=110).suffix(" °C"));
            let mut off = p.rpm == 0;
            ui.horizontal(|ui| {
                if !off {
                    ui.add(egui::DragValue::new(&mut p.rpm).range(MIN_RPM..=MAX_RPM[fan]).speed(10.0));
                }
                if ui.checkbox(&mut off, "off").changed() {
                    p.rpm = if off { 0 } else { 1200.min(MAX_RPM[fan]) };
                }
            });
            if ui.small_button("✕").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove.filter(|_| d.points.len() > 1) {
        d.points.remove(i);
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(d.points.len() < LEVELS, egui::Button::new("+ point")).clicked() {
            let last = d.points.iter().max_by_key(|p| p.temp).copied();
            let p = last.map_or(CurvePoint { temp: 50, rpm: 1500 }, |l| CurvePoint {
                temp: l.temp.saturating_add(5).min(110),
                rpm: (l.rpm + 200).min(MAX_RPM[fan]),
            });
            d.points.push(p);
        }
        if ui.button("sort").clicked() {
            d.points.sort_by_key(|p| p.temp);
        }
        let mut temps: Vec<u8> = d.points.iter().map(|p| p.temp).collect();
        temps.sort_unstable();
        temps.dedup();
        if temps.len() != d.points.len() {
            ui.colored_label(WARN, "points share a temperature: they switch together");
        }
    });
}

enum CurveEdit {
    DragStart(usize),
    Drag(CurvePoint),
    DragEnd,
    Add(CurvePoint),
    Remove(usize),
}

/// Step plot of a curve, with the live temperature and speed. Returns an
/// edit if `editable` and the user dragged, added or removed a point.
fn curve_plot(
    ui: &mut Ui,
    fan: usize,
    pts: &[CurvePoint],
    dragging: Option<usize>,
    f: &vfc::proto::FanStatus,
    editable: bool,
) -> Option<CurveEdit> {
    let max = f64::from(MAX_RPM[fan]);
    let mut sorted: Vec<CurvePoint> = pts.to_vec();
    sorted.sort_by_key(|p| p.temp);
    let mut steps = Vec::new();
    let idle = Some(0.0);
    if let (Some(first), Some(y)) = (sorted.first(), idle) {
        steps.push([20.0, y]);
        steps.push([f64::from(first.temp), y]);
    }
    for (i, p) in sorted.iter().enumerate() {
        let next = sorted.get(i + 1).map_or(110.0, |n| f64::from(n.temp));
        steps.push([f64::from(p.temp), f64::from(p.rpm)]);
        steps.push([next, f64::from(p.rpm)]);
    }
    let marks: Vec<[f64; 2]> = pts.iter().map(|p| [f64::from(p.temp), f64::from(p.rpm)]).collect();
    let color = FAN_COLOR[fan];
    let plot = Plot::new(("curve", fan))
        .height(260.0)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .sense(Sense::click_and_drag())
        .x_axis_formatter(|m, _| format!("{:.0}°", m.value))
        .y_axis_formatter(|m, _| format!("{:.0}", m.value));
    let resp = plot.show(ui, |p| {
        p.set_plot_bounds_x(20.0..=110.0);
        p.set_plot_bounds_y(0.0..=max + 200.0);
        p.line(Line::new("curve", PlotPoints::new(steps)).color(color).width(2.0).allow_band(false));
        if editable {
            p.points(Points::new("points", PlotPoints::new(marks.clone())).radius(6.0).color(color).filled(true));
        }
        p.vline(VLine::new("now", f64::from(f.temp)).color(CPU_COLOR.gamma_multiply(0.7)));
        p.points(
            Points::new("live", PlotPoints::new(vec![[f64::from(f.temp), f64::from(f.rpm)]]))
                .shape(MarkerShape::Diamond)
                .radius(7.0)
                .color(Color32::WHITE),
        );
        let pointer = p.pointer_coordinate();
        let nearest = p.response().hover_pos().and_then(|pos| {
            marks
                .iter()
                .enumerate()
                .map(|(i, m)| (i, p.screen_from_plot(egui_plot::PlotPoint::new(m[0], m[1])).distance(pos)))
                .filter(|(_, d)| *d < 14.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        });
        (pointer, nearest)
    });
    if !editable {
        return None;
    }
    let (pointer, nearest) = resp.inner;
    let r = &resp.response;
    let to_point = |pp: egui_plot::PlotPoint| CurvePoint {
        temp: pp.x.round().clamp(0.0, 110.0) as u8,
        rpm: {
            let v = ((pp.y / 50.0).round() * 50.0).clamp(0.0, max) as u32;
            if v < MIN_RPM / 2 { 0 } else { v.max(MIN_RPM) }
        },
    };
    if nearest.is_some() || dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if r.drag_started() {
        return nearest.map(CurveEdit::DragStart);
    }
    if r.drag_stopped() {
        return Some(CurveEdit::DragEnd);
    }
    if r.dragged() && dragging.is_some() {
        return pointer.map(|pp| CurveEdit::Drag(to_point(pp)));
    }
    if r.double_clicked() && nearest.is_none() {
        return pointer.map(|pp| CurveEdit::Add(to_point(pp)));
    }
    if r.secondary_clicked() {
        return nearest.map(CurveEdit::Remove);
    }
    None
}

/// Decoded per-level editor for one fan's rows.
fn table_grid(ui: &mut Ui, fan: usize, edit: &mut [u8], base: &[u8], level: u8) {
    let at = |addr: u16| (addr - tables::TABLES_START) as usize;
    let r = ROWS[fan];
    egui::Grid::new(("tbl", fan)).striped(true).spacing([4.0, 2.0]).show(ui, |ui| {
        ui.weak("level");
        for i in 0..LEVELS {
            let txt = RichText::new(format!("{}", i + 1)).monospace();
            if usize::from(level) == i + 1 {
                ui.label(txt.strong().color(FAN_COLOR[fan]));
            } else {
                ui.weak(txt);
            }
        }
        ui.end_row();
        let byte_rows = [
            ("up °C", r.up, "Level rises once the temperature reaches this (0 = never)"),
            ("hyst °C", r.hyst, "Level drops below up - hyst (after ~20-30 s)"),
            ("low", r.low, "Unknown threshold row; must not be 0 (0 drops the fan to level 0)"),
            ("duty", r.duty, "Starting PWM duty (EC trims it toward the target)"),
            ("? row", r.unknown, "Unknown, firmware writes 9, 2, 2, ... (drop delay?)"),
        ];
        for (name, row, tip) in byte_rows {
            ui.label(name).on_hover_text(format!("0x{row:04X}: {tip}"));
            for i in 0..LEVELS {
                let a = at(row + i as u16);
                byte_cell(ui, &mut edit[a], base[a], false);
            }
            ui.end_row();
        }
        ui.label("target tach").on_hover_text(format!("0x{:04X}: u16 LE tach setpoint (smaller = faster)", r.target));
        for i in 0..LEVELS {
            let a = at(r.target + 2 * i as u16);
            let mut w = u16::from_le_bytes([edit[a], edit[a + 1]]);
            let orig = u16::from_le_bytes([base[a], base[a + 1]]);
            let resp = ui.add(egui::DragValue::new(&mut w).speed(2.0));
            if w != orig {
                resp.highlight();
            }
            [edit[a], edit[a + 1]] = w.to_le_bytes();
        }
        ui.end_row();
        ui.weak("= rpm");
        for i in 0..LEVELS {
            let a = at(r.target + 2 * i as u16);
            let rpm = tables::tach_to_rpm(u16::from_le_bytes([edit[a], edit[a + 1]]));
            ui.weak(if rpm == 0 { "off".into() } else { rpm.to_string() });
        }
        ui.end_row();
    });
}

fn byte_cell(ui: &mut Ui, v: &mut u8, orig: u8, hex: bool) {
    let mut dv = egui::DragValue::new(v).speed(0.3);
    if hex {
        dv = dv.hexadecimal(2, false, false);
    }
    let resp = ui.add(dv);
    if *v != orig {
        resp.highlight();
    }
}

fn hex_editor(ui: &mut Ui, edit: &mut [u8], base: &[u8]) {
    egui::Grid::new("hexed").spacing([3.0, 2.0]).show(ui, |ui| {
        ui.label("");
        for c in 0..16 {
            ui.weak(format!("{c:X}"));
        }
        ui.end_row();
        for row in 0..16 {
            let addr = tables::TABLES_START + row * 16;
            ui.monospace(RichText::new(format!("{addr:04X}")).weak())
                .on_hover_text(known_name(addr).unwrap_or("unused / unknown"));
            for c in 0..16 {
                let i = usize::from(row * 16 + c);
                byte_cell(ui, &mut edit[i], base[i], true);
            }
            ui.end_row();
        }
    });
}

/// Contiguous runs of changed offsets: (start offset, bytes).
fn runs(changed: &[usize], data: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let mut out: Vec<(usize, Vec<u8>)> = Vec::new();
    for &i in changed {
        match out.last_mut() {
            Some((s, b)) if *s + b.len() == i => b.push(data[i]),
            _ => out.push((i, vec![data[i]])),
        }
    }
    out
}

fn known_name(addr: u16) -> Option<&'static str> {
    Some(match addr {
        0x03E8 | 0x03E9 => "fan0 tach (u16 LE)",
        0x03EA | 0x03EB => "fan1 tach (u16 LE)",
        0x03EC => "fan0 level",
        0x03ED => "fan1 level",
        0x0576 => "fan0 control temp",
        0x0577 => "fan1 control temp",
        0x05C2 | 0x05C3 | 0x05C5 => "limit? (written by firmware)",
        0x08F4 => "profile mode code (1 std, 2 whisper, 3 perf, 4 full)",
        0x0BE7 | 0x0BEA => "FANL commit flag",
        a if (0x0A00..0x0B00).contains(&a) => match tables::owner(a) {
            Some(0) => "fan0 table row",
            Some(_) => "fan1 table row",
            None => "shared/unknown table row",
        },
        _ => return None,
    })
}

fn log(ui: &mut Ui, st: &Status) {
    ui.label(RichText::new("Daemon events").strong());
    egui::Grid::new("log").striped(true).show(ui, |ui| {
        for (t, msg) in st.events.iter().rev() {
            let secs = now_secs() as u64 - t.min(&(now_secs() as u64));
            ui.weak(ago(secs));
            let color = if msg.starts_with("error") || msg.starts_with("guard:") { BAD } else { ui.visuals().text_color() };
            ui.colored_label(color, msg);
            ui.end_row();
        }
    });
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        _ => format!("{}h {}m ago", secs / 3600, secs / 60 % 60),
    }
}
