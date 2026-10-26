//! EC access through the firmware's own ACPI methods.
//!
//! `RRAM`/`WRAM` go through the ECW2 mailbox under the firmware's `MUEC`
//! mutex, so they can't race with the firmware's own EC traffic. Writes are
//! limited to the fan table block plus FANL's two commit flags.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::acpi_call::{self, ONES};

const EC: &str = r"\_SB.PC00.LPCB.EC0";

/// Fan table block rewritten by FANL on every profile change.
pub const TABLES: (u16, u16) = (0x0A00, 0x100);

/// Fan tach words (ECW1 F0TS/F1TS, i.e. EC RAM 0x0380 + 0x68/0x6A).
/// 0 = stopped; otherwise a period-like count, smaller = faster.
pub const TACH: [u16; 2] = [0x03E8, 0x03EA];

/// FS_C's constant: RPM = 0x1C8402 / (2 * tach).
const RPM_K: u64 = 0x1C8402 / 2;

/// Current level (0-13) per fan.
pub const LEVEL: [u16; 2] = [0x03EC, 0x03ED];
/// EC control temperature (°C) per fan.
pub const CTL_TEMP: [u16; 2] = [0x0576, 0x0577];

/// EC RAM registers ACPI reads or writes around fan control (see NOTES.md).
pub const FAN_REGS: &[(&str, u16)] = &[
    ("fan0 level  ", 0x03EC),
    ("fan1 level  ", 0x03ED),
    ("fan0 temp   ", 0x0576),
    ("fan1 temp   ", 0x0577),
    ("profile code", 0x08F4),
    ("limit? 05C2 ", 0x05C2),
    ("limit? 05C3 ", 0x05C3),
    ("limit? 05C5 ", 0x05C5),
];

pub fn rram(addr: u16) -> Result<u8> {
    let v = acpi_call::call(&format!("{EC}.RRAM 0x{addr:04x}"))?;
    if v == ONES || v == u64::from(u32::MAX) {
        bail!("RRAM 0x{addr:04x}: EC mailbox timeout");
    }
    Ok(v as u8)
}

/// Commit flags FANL sets after rewriting the tables:
/// `WRAM(0x0BEA, 1)` and `0x0BE7 |= 1`.
const APPLY_FLAG: u16 = 0x0BEA;
const APPLY_BITS: u16 = 0x0BE7;

/// Profile mode code FANL writes first (1=standard 2=whisper 3=perf 4=full).
pub const MODE: u16 = 0x08F4;

fn writable(addr: u16, value: u8) -> bool {
    (TABLES.0..TABLES.0 + TABLES.1).contains(&addr) || (addr == MODE && (1..=4).contains(&value))
}

/// Write one fan table byte (or the FANL mode code) and read it back.
pub fn wram(addr: u16, value: u8) -> Result<()> {
    if !writable(addr, value) {
        bail!(
            "refusing to write 0x{value:02x} to 0x{addr:04x}: only the fan table block \
             0x{:04x}-0x{:04x} and mode codes 1-4 at 0x{MODE:04x} are writable",
            TABLES.0,
            TABLES.0 + TABLES.1 - 1
        );
    }
    wram_unchecked(addr, value)
}

fn wram_unchecked(addr: u16, value: u8) -> Result<()> {
    let r = acpi_call::call(&format!("{EC}.WRAM 0x{addr:04x} 0x{value:02x}"))?;
    if r != 1 {
        bail!("WRAM 0x{addr:04x}: EC mailbox timeout");
    }
    let back = rram(addr)?;
    if back != value {
        bail!("WRAM 0x{addr:04x}: wrote 0x{value:02x}, read back 0x{back:02x}");
    }
    Ok(())
}

/// Write a little-endian u16 table entry (tach targets).
pub fn wram_u16(addr: u16, value: u16) -> Result<()> {
    let [lo, hi] = value.to_le_bytes();
    wram(addr, lo)?;
    wram(addr + 1, hi)
}

fn wram_flag(addr: u16, value: u8) -> Result<()> {
    if acpi_call::call(&format!("{EC}.WRAM 0x{addr:04x} 0x{value:02x}"))? != 1 {
        bail!("WRAM 0x{addr:04x}: EC mailbox timeout");
    }
    Ok(())
}

/// Set the same commit flags FANL sets after loading new tables. Not read
/// back: the EC may clear them once it has picked up the new tables.
///
/// With `edge`, clear both first so the EC sees a 0 -> 1 transition; FANL
/// itself never writes 0 here, so this is an experiment, not firmware behaviour.
pub fn apply(edge: bool) -> Result<()> {
    if edge {
        wram_flag(APPLY_FLAG, 0)?;
        let bits = rram(APPLY_BITS)?;
        wram_flag(APPLY_BITS, bits & !1)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    wram_flag(APPLY_FLAG, 1)?;
    let bits = rram(APPLY_BITS)?;
    wram_flag(APPLY_BITS, bits | 1)
}

pub fn read_u16(addr: u16) -> Result<u16> {
    Ok(u16::from_le_bytes([rram(addr)?, rram(addr + 1)?]))
}

pub fn tach(fan: usize) -> Result<u16> {
    read_u16(TACH[fan])
}

/// Fan speed in RPM from the raw tach word.
///
/// The firmware's own `FS_C` inverts the word first (`~F0TS`), which this EC
/// doesn't expect: a real reading of 611 becomes 64924 and FS_C reports
/// 14 RPM. So we read the tach directly and skip the inversion.
pub fn fan_rpm(fan: usize) -> Result<u64> {
    Ok(match tach(fan)? {
        0 => 0,
        t => RPM_K / u64::from(t),
    })
}

pub fn hexdump(start: u16, len: u16) -> Result<String> {
    let mut out = String::new();
    for row in (start..start + len).step_by(16) {
        write!(out, "0x{row:04X}:")?;
        for addr in row..(row + 16).min(start + len) {
            write!(out, " {:02x}", rram(addr)?)?;
        }
        out.push('\n');
    }
    Ok(out)
}

/// Read a raw EC RAM range. Addresses the EC doesn't answer read as 0xFF;
/// their count is returned alongside the bytes.
pub fn scan(start: u16, len: u16) -> (Vec<u8>, usize) {
    let mut failed = 0;
    let bytes = (start..start + len)
        .map(|addr| {
            rram(addr).unwrap_or_else(|_| {
                failed += 1;
                0xFF
            })
        })
        .collect();
    (bytes, failed)
}

pub fn fans() -> Result<String> {
    let mut out = String::new();
    for fan in 0..2 {
        writeln!(out, "fan{fan} rpm     : {:4}  (tach {})", fan_rpm(fan)?, tach(fan)?)?;
    }
    for (name, addr) in FAN_REGS {
        writeln!(out, "{name}: {:3}  (0x{addr:04X})", rram(*addr)?)?;
    }
    Ok(out)
}

/// Read a range, failing on the first address the EC doesn't answer.
pub fn read_block(start: u16, len: u16) -> Result<Vec<u8>> {
    (start..start + len).map(rram).collect()
}
