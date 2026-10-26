//! vfcd's socket protocol: one JSON request per line, one JSON response
//! per line. The socket is root:<group> 0660, so only that group can talk
//! to the daemon at all.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::sys::Battery;
use crate::tables::FanMode;

pub const SOCKET: &str = "/run/vfcd.sock";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guard {
    pub enabled: bool,
    /// Override custom modes with full speed at this temperature (°C)...
    pub trip: u8,
    /// ...until everything is back below this one.
    pub release: u8,
}

impl Default for Guard {
    fn default() -> Self {
        Self { enabled: true, trip: 95, release: 80 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Control {
    pub fans: [FanMode; 2],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FanStatus {
    pub rpm: u32,
    pub tach: u16,
    pub level: u8,
    /// EC control temperature for this fan.
    pub temp: u8,
    /// Tach target of the active level, as RPM.
    pub target_rpm: Option<u32>,
    /// Duty of the active level (the EC trims it).
    pub duty: Option<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub fans: [FanStatus; 2],
    pub cpu_temp: Option<f32>,
    pub ec_mode: u8,
    pub profile: String,
    pub profiles: Vec<String>,
    pub control: Control,
    pub boot: Control,
    pub guard: Guard,
    pub guard_active: bool,
    pub ac: bool,
    pub battery: Battery,
    pub dgpu: String,
    /// Last full read of 0x0A00-0x0AFF.
    pub tables: Vec<u8>,
    /// Recent daemon events, oldest first: (unix seconds, message).
    pub events: Vec<(u64, String)>,
    /// Milliseconds per EC byte read, averaged (diagnostic).
    pub ec_read_ms: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Status,
    /// Fresh read of the table block.
    Tables,
    /// Read any EC RAM range (max 0x1000 bytes).
    ReadEc { addr: u16, len: u16 },
    /// Write table bytes; fans whose rows are touched switch to Manual.
    WriteTable { addr: u16, bytes: Vec<u8> },
    SetControl(Control),
    /// Store this control as the one applied at daemon start.
    SetBoot(Control),
    SetGuard(Guard),
    SetProfile(String),
    SetChargeLimit(u8),
    Dgpu(String),
    /// Firmware tables for both fans (profile toggle), control = stock.
    Restore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Message(String),
    Status(Box<Status>),
    Bytes(Vec<u8>),
    Error(String),
}

/// A connection to vfcd.
pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let s = UnixStream::connect(SOCKET).with_context(|| format!("connecting to {SOCKET}"))?;
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(Self { reader: BufReader::new(s) })
    }

    pub fn request(&mut self, req: &Request) -> Result<Response> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        self.reader.get_mut().write_all(line.as_bytes())?;
        let mut resp = String::new();
        if self.reader.read_line(&mut resp)? == 0 {
            bail!("vfcd closed the connection");
        }
        match serde_json::from_str(&resp)? {
            Response::Error(e) => bail!("{e}"),
            r => Ok(r),
        }
    }
}
