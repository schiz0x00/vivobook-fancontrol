//! vfc — Vivobook fan control research tool.

mod acpi_call;
mod dsdt;
mod ec;

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "ASUS Vivobook Pro 15 (N6506MU) EC fan research tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the per-profile fan tables written by the DSDT
    Extract {
        #[arg(default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/acpi/tables/DSDT.dsl"))]
        dsl: PathBuf,
    },
    /// Hex dump EC RAM (read-only, needs root + acpi_call)
    Read {
        #[arg(value_parser = parse_u16)]
        addr: u16,
        #[arg(value_parser = parse_u16, default_value = "1")]
        len: u16,
    },
    /// Fan RPM and fan-related EC registers
    Fans,
    /// Dump the live fan table block (0x0A00-0x0AFF)
    Tables,
    /// Save fans + tables + platform profile to dumps/NAME.txt
    Snapshot { name: String },
    /// Raw dump of EC RAM to dumps/NAME.bin (default 0x0000-0x0FFF)
    Scan {
        name: String,
        #[arg(long, value_parser = parse_u16, default_value = "0")]
        start: u16,
        #[arg(long, value_parser = parse_u16, default_value = "0x1000")]
        len: u16,
    },
    /// Write a fan table byte (0x0A00-0x0AFF only), read back to verify
    Write {
        #[arg(value_parser = parse_u16)]
        addr: u16,
        #[arg(value_parser = parse_u16)]
        value: u16,
        /// Write VALUE as a little-endian u16 at ADDR, ADDR+1 (tach targets)
        #[arg(long)]
        word: bool,
    },
    /// Set FANL's commit flags so the EC picks up modified tables
    Apply {
        /// Clear the flags first to produce a 0 -> 1 edge
        #[arg(long)]
        edge: bool,
    },
    /// Make the firmware rewrite all fan tables (toggle platform_profile)
    Restore,
    /// Show bytes that differ between raw scans, with 16-bit LE values
    Diff {
        /// Scans to compare (dumps/NAME.bin or paths)
        #[arg(required = true, num_args = 2..)]
        scans: Vec<String>,
        #[arg(long, value_parser = parse_u16, default_value = "0")]
        start: u16,
    },
}

fn dumps_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dumps")
}

fn scan_path(name: &str) -> PathBuf {
    let p = PathBuf::from(name);
    if p.exists() { p } else { dumps_dir().join(format!("{name}.bin")) }
}

/// Print every address whose byte differs across scans, plus the 16-bit
/// little-endian word starting there (tach counters are usually 16-bit).
fn diff(names: &[String], start: u16) -> Result<()> {
    let scans = names
        .iter()
        .map(|n| fs::read(scan_path(n)).with_context(|| format!("reading scan {n}")))
        .collect::<Result<Vec<_>>>()?;
    let len = scans.iter().map(Vec::len).min().unwrap_or(0);

    print!("addr  ");
    for n in names {
        print!(" {n:>14}");
    }
    println!();
    for i in 0..len {
        if scans.iter().all(|s| s[i] == scans[0][i]) {
            continue;
        }
        print!("0x{:04X}", start as usize + i);
        for s in &scans {
            let word = s.get(i + 1).map(|hi| u16::from_le_bytes([s[i], *hi]));
            match word {
                Some(w) => print!("   {:02x} (w{w:>5})", s[i]),
                None => print!("   {:02x}         ", s[i]),
            }
        }
        println!();
    }
    Ok(())
}

fn parse_u16(s: &str) -> Result<u16, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16),
        None => s.parse(),
    };
    r.map_err(|e| e.to_string())
}

fn require_acpi_call() -> Result<()> {
    if !acpi_call::available() {
        bail!("acpi_call not loaded (/proc/acpi/call missing)");
    }
    Ok(())
}

const PROFILE: &str = "/sys/firmware/acpi/platform_profile";

fn platform_profile() -> String {
    fs::read_to_string(PROFILE)
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "?".into())
}

fn main() -> Result<()> {
    // Die quietly on a closed pipe (`vfc read ... | head`) like other CLI tools.
    // SAFETY: called before any threads exist; SIG_DFL is always a valid handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

    match Cli::parse().cmd {
        Cmd::Extract { dsl } => {
            let src = fs::read_to_string(&dsl).with_context(|| format!("reading {}", dsl.display()))?;
            print!("{}", dsdt::fan_tables(&src)?);
        }
        Cmd::Read { addr, len } => {
            require_acpi_call()?;
            print!("{}", ec::hexdump(addr, len)?);
        }
        Cmd::Fans => {
            require_acpi_call()?;
            print!("{}", ec::fans()?);
        }
        Cmd::Tables => {
            require_acpi_call()?;
            let (start, len) = ec::TABLES;
            print!("{}", ec::hexdump(start, len)?);
        }
        Cmd::Scan { name, start, len } => {
            require_acpi_call()?;
            let (bytes, failed) = ec::scan(start, len);
            let path = scan_path(&name);
            fs::create_dir_all(dumps_dir())?;
            fs::write(&path, bytes)?;
            println!("wrote {} ({failed} unreadable addresses)", path.display());
        }
        Cmd::Diff { scans, start } => diff(&scans, start)?,
        Cmd::Write { addr, value, word } => {
            require_acpi_call()?;
            if word {
                ec::wram_u16(addr, value)?;
                println!("0x{addr:04X} <- {value} (u16 LE)");
            } else {
                let byte = u8::try_from(value).context("value must fit in a byte (or use --word)")?;
                ec::wram(addr, byte)?;
                println!("0x{addr:04X} <- {byte}");
            }
        }
        Cmd::Apply { edge } => {
            require_acpi_call()?;
            ec::apply(edge)?;
            println!("commit flags set{}", if edge { " (0 -> 1 edge)" } else { "" });
        }
        Cmd::Restore => {
            let profile = platform_profile();
            let other = if profile == "balanced" { "quiet" } else { "balanced" };
            for p in [other, profile.as_str()] {
                fs::write(PROFILE, p).with_context(|| format!("setting platform_profile={p}"))?;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            println!("platform_profile {other} -> {profile}: firmware rewrote the fan tables");
        }
        Cmd::Snapshot { name } => {
            require_acpi_call()?;
            let dir = dumps_dir();
            fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{name}.txt"));
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let (start, len) = ec::TABLES;
            fs::write(
                &path,
                format!(
                    "# unix={now} platform_profile={}\n{}\n{}",
                    platform_profile(),
                    ec::fans()?,
                    ec::hexdump(start, len)?
                ),
            )?;
            println!("wrote {}", path.display());
        }
    }
    Ok(())
}
