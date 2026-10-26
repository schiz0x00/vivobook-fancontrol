//! Minimal wrapper around the `acpi_call` kernel module's `/proc/acpi/call`.

use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};

const PROC: &str = "/proc/acpi/call";
const BOARD: &str = "/sys/class/dmi/id/board_name";

/// The only board whose EC layout this was mapped on.
pub const SUPPORTED_BOARD: &str = "N6506MU";
/// Set to 1 to run on another board anyway (for people mapping a sibling model).
pub const FORCE_ENV: &str = "VFC_FORCE_UNSUPPORTED";

/// ACPI `Ones` as returned on a 64-bit (revision 2+) DSDT.
pub const ONES: u64 = u64::MAX;

pub fn available() -> bool {
    Path::new(PROC).exists()
}

/// Refuse to touch the EC on anything but the mapped board: the same
/// addresses mean something else on other firmware.
pub fn check_board() -> Result<()> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            let board = fs::read_to_string(BOARD).map(|s| s.trim().to_owned()).unwrap_or_default();
            if board == SUPPORTED_BOARD || std::env::var(FORCE_ENV).is_ok_and(|v| v == "1") {
                Ok(())
            } else {
                Err(format!(
                    "unsupported board {board:?}: this tool only knows the EC of the ASUS Vivobook Pro 15 \
                     {SUPPORTED_BOARD}; writing it on other hardware is unsafe (set {FORCE_ENV}=1 to override)"
                ))
            }
        })
        .clone()
        .map_err(|e| anyhow!(e))
}

/// Evaluate an ACPI method, e.g. `\_SB.PC00.LPCB.EC0.RRAM 0x0a00`.
/// Only integer results are supported, which is all the EC methods return.
pub fn call(expr: &str) -> Result<u64> {
    check_board()?;
    // The proc file holds one pending result: a write and the read that
    // follows must not interleave with another thread's call.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::write(PROC, expr).with_context(|| format!("writing {PROC} (root + acpi_call needed)"))?;
    let raw = fs::read(PROC).with_context(|| format!("reading {PROC}"))?;
    let out = String::from_utf8_lossy(&raw);
    let out = out.trim_end_matches('\0').trim();

    if out.starts_with("Error") {
        bail!("{expr}: {out}");
    }
    let hex = out.strip_prefix("0x").with_context(|| format!("{expr}: non-integer result {out:?}"))?;
    u64::from_str_radix(hex, 16).with_context(|| format!("{expr}: bad integer {out:?}"))
}
