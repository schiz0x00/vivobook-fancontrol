//! Plain sysfs knobs around the fans: CPU temperature, platform profile,
//! AC/battery state, charge limit and the dGPU power script.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const PROFILE: &str = "/sys/firmware/acpi/platform_profile";
const PROFILE_CHOICES: &str = "/sys/firmware/acpi/platform_profile_choices";
const BAT: &str = "/sys/class/power_supply/BAT0";
const DGPU: &str = "/sys/bus/pci/devices/0000:01:00.0";
pub const DGPU_SCRIPT: &str = "/usr/local/sbin/dgpu-power";

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn read_num<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read(path)?.parse().ok()
}

pub fn platform_profile() -> String {
    read(PROFILE).unwrap_or_else(|| "?".into())
}

pub fn profile_choices() -> Vec<String> {
    read(PROFILE_CHOICES)
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

pub fn set_platform_profile(p: &str) -> Result<()> {
    if !profile_choices().iter().any(|c| c == p) {
        bail!("unknown platform profile {p:?}");
    }
    fs::write(PROFILE, p).with_context(|| format!("setting platform_profile={p}"))
}

/// Make the firmware rewrite every fan table: switch profile away and back.
pub fn restore_tables() -> Result<()> {
    let profile = platform_profile();
    let other = if profile == "balanced" { "quiet" } else { "balanced" };
    for p in [other, profile.as_str()] {
        fs::write(PROFILE, p).with_context(|| format!("setting platform_profile={p}"))?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok(())
}

/// Package temperature from coretemp, in °C.
pub fn cpu_temp() -> Option<f32> {
    let dir = fs::read_dir("/sys/class/hwmon").ok()?.flatten().find(|e| {
        read(e.path().join("name")).as_deref() == Some("coretemp")
    })?;
    read_num::<f32>(dir.path().join("temp1_input")).map(|m| m / 1000.0)
}

pub fn ac_online() -> bool {
    fs::read_dir("/sys/class/power_supply")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            read(e.path().join("type")).as_deref() == Some("Mains")
                && read(e.path().join("online")).as_deref() == Some("1")
        })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Battery {
    pub capacity: Option<u8>,
    pub status: String,
    /// Watts, positive either way (see `status` for direction).
    pub power: Option<f32>,
    pub charge_limit: Option<u8>,
}

pub fn battery() -> Battery {
    Battery {
        capacity: read_num(format!("{BAT}/capacity")),
        status: read(format!("{BAT}/status")).unwrap_or_default(),
        power: read_num::<f32>(format!("{BAT}/power_now")).map(|uw| uw / 1e6),
        charge_limit: read_num(format!("{BAT}/charge_control_end_threshold")),
    }
}

pub fn set_charge_limit(pct: u8) -> Result<()> {
    if !(20..=100).contains(&pct) {
        bail!("charge limit must be 20-100%");
    }
    fs::write(format!("{BAT}/charge_control_end_threshold"), pct.to_string())
        .context("setting charge_control_end_threshold")
}

/// dGPU state as reported by the PCI device: "removed", "suspended", "active".
pub fn dgpu_state() -> String {
    if !Path::new(DGPU).exists() {
        return "removed (rail off)".into();
    }
    read(format!("{DGPU}/power/runtime_status")).unwrap_or_else(|| "?".into())
}

/// Run the dgpu-power script with on/off/auto; returns its output.
pub fn dgpu_power(action: &str) -> Result<String> {
    if !["on", "off", "auto"].contains(&action) {
        bail!("dgpu action must be on, off or auto");
    }
    let out = Command::new(DGPU_SCRIPT)
        .arg(action)
        .output()
        .with_context(|| format!("running {DGPU_SCRIPT}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("{DGPU_SCRIPT} {action} failed: {}", text.trim());
    }
    Ok(text.trim().to_owned())
}
