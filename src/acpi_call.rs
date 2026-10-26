//! Minimal wrapper around the `acpi_call` kernel module's `/proc/acpi/call`.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

const PROC: &str = "/proc/acpi/call";

/// ACPI `Ones` as returned on a 64-bit (revision 2+) DSDT.
pub const ONES: u64 = u64::MAX;

pub fn available() -> bool {
    Path::new(PROC).exists()
}

/// Evaluate an ACPI method, e.g. `\_SB.PC00.LPCB.EC0.RRAM 0x0a00`.
/// Only integer results are supported, which is all the EC methods return.
pub fn call(expr: &str) -> Result<u64> {
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
    let hex = out
        .strip_prefix("0x")
        .with_context(|| format!("{expr}: non-integer result {out:?}"))?;
    u64::from_str_radix(hex, 16).with_context(|| format!("{expr}: bad integer {out:?}"))
}
