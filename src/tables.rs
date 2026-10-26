//! Decoding and encoding the EC fan table block (0x0A00-0x0AFF).
//!
//! Per fan, 13 levels. Measured behaviour (NOTES.md, tests 6-9):
//! - The level climbs to the number of up thresholds the fan's control
//!   temperature has passed, on the next temperature change.
//! - Level L drops once the temperature is below `up[L-1] - hyst[L-1]`
//!   (row 0x0A10/0x0A50), applied with a delay of ~20-30 s.
//! - Row 0x0A20/0x0A60 ("low") is not the drop threshold, but a 0 there
//!   drops the fan to level 0 on every temperature decrease; an up
//!   threshold of 0 is never passed. Neither row may contain 0.
//! - Level N uses entry N-1 of the duty and tach target rows; level 0 uses
//!   none and the fan stops. The tach target is the setpoint; the duty
//!   entry is only where the EC starts before trimming towards it.

use serde::{Deserialize, Serialize};

use crate::ec::TABLES;

pub const LEVELS: usize = 13;
pub const TABLES_START: u16 = TABLES.0;

/// Row addresses for one fan.
#[derive(Clone, Copy, Debug)]
pub struct Rows {
    pub up: u16,
    pub hyst: u16,
    pub low: u16,
    pub duty: u16,
    /// 13 little-endian u16 tach targets.
    pub target: u16,
    /// Firmware writes 9, 2, 2, ...; meaning unknown (drop delay?).
    pub unknown: u16,
}

pub const ROWS: [Rows; 2] = [
    Rows { up: 0x0A00, hyst: 0x0A10, low: 0x0A20, duty: 0x0A80, target: 0x0AC0, unknown: 0x0A30 },
    Rows { up: 0x0A40, hyst: 0x0A50, low: 0x0A60, duty: 0x0AA0, target: 0x0AE0, unknown: 0x0A70 },
];

/// Shared rows with no known owner (`01 01 02 03` repeated).
pub const SHARED_UNKNOWN: [u16; 2] = [0x0A90, 0x0AB0];

/// RPM = RPM_K / tach (FS_C's constant without its bogus inversion).
pub const RPM_K: u32 = 0x1C8402 / 2;

/// Roughly what the firmware's own "full" profile asks for (duty 255 /
/// tach 371 on fan0, duty 226 / tach 308 on fan1), rounded up a little.
pub const MAX_RPM: [u32; 2] = [2700, 3200];
/// Below this the fans may stall; 0 (off) is allowed separately.
pub const MIN_RPM: u32 = 500;
/// Lowest up threshold written: "always on" is every threshold at 20 °C,
/// well below any real EC temperature (0 is never passed, see above).
pub const MIN_THRESHOLD: u8 = 20;
/// Lowest value for the low row and for `up - hyst`.
pub const MIN_DOWN: u8 = 10;
/// Measured duty per RPM, used only for the EC's starting point.
const RPM_PER_DUTY: [u32; 2] = [11, 14];

pub fn tach_to_rpm(tach: u16) -> u32 {
    if tach == 0 { 0 } else { RPM_K / u32::from(tach) }
}

pub fn rpm_to_tach(rpm: u32) -> u16 {
    RPM_K.checked_div(rpm).map_or(0, |t| t.min(u32::from(u16::MAX)) as u16)
}

pub fn duty_for(fan: usize, rpm: u32) -> u8 {
    (rpm / RPM_PER_DUTY[fan]).min(255) as u8
}

/// Which fan's rows contain `addr`, if any.
pub fn owner(addr: u16) -> Option<usize> {
    (0..2).find(|&f| {
        let r = ROWS[f];
        [r.up, r.hyst, r.low, r.duty, r.unknown].iter().any(|&row| (row..row + 16).contains(&addr))
            || (r.target..r.target + 32).contains(&addr)
    })
}

/// One fan's decoded table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FanTable {
    pub up: [u8; LEVELS],
    pub hyst: [u8; LEVELS],
    pub low: [u8; LEVELS],
    pub duty: [u8; LEVELS],
    pub target: [u16; LEVELS],
}

impl FanTable {
    /// Decode from the 256-byte block starting at 0x0A00.
    pub fn decode(block: &[u8], fan: usize) -> Option<Self> {
        if block.len() < TABLES.1 as usize {
            return None;
        }
        let r = ROWS[fan];
        let at = |addr: u16| block[(addr - TABLES.0) as usize];
        let mut t = Self::default();
        for i in 0..LEVELS {
            let i16 = i as u16;
            t.up[i] = at(r.up + i16);
            t.hyst[i] = at(r.hyst + i16);
            t.low[i] = at(r.low + i16);
            t.duty[i] = at(r.duty + i16);
            t.target[i] = u16::from_le_bytes([at(r.target + 2 * i16), at(r.target + 2 * i16 + 1)]);
        }
        Some(t)
    }

    /// Bytes the daemon enforces: thresholds and targets. The duty row is
    /// left out because the EC trims the active entry itself.
    pub fn enforced(&self, fan: usize) -> Vec<(u16, u8)> {
        let r = ROWS[fan];
        let mut out = Vec::with_capacity(5 * LEVELS);
        for i in 0..LEVELS {
            let i16 = i as u16;
            out.push((r.up + i16, self.up[i]));
            out.push((r.hyst + i16, self.hyst[i]));
            out.push((r.low + i16, self.low[i]));
            let [lo, hi] = self.target[i].to_le_bytes();
            out.push((r.target + 2 * i16, lo));
            out.push((r.target + 2 * i16 + 1, hi));
        }
        out
    }

    pub fn duty_bytes(&self, fan: usize) -> Vec<(u16, u8)> {
        (0..LEVELS).map(|i| (ROWS[fan].duty + i as u16, self.duty[i])).collect()
    }

    /// Every byte of this table, duty first so the EC starts from it.
    pub fn all_bytes(&self, fan: usize) -> Vec<(u16, u8)> {
        let mut v = self.duty_bytes(fan);
        v.extend(self.enforced(fan));
        v
    }

    /// Target RPM for a level (None at level 0 or out of range).
    pub fn level_rpm(&self, level: u8) -> Option<u32> {
        let i = usize::from(level).checked_sub(1)?;
        self.target.get(i).map(|&t| tach_to_rpm(t))
    }

    /// The same speed at every level; every threshold is passed from 20 °C
    /// and only dropped below 10 °C.
    pub fn fixed(fan: usize, rpm: u32) -> Self {
        let rpm = clamp_rpm(fan, rpm);
        Self {
            up: [MIN_THRESHOLD; LEVELS],
            hyst: [MIN_THRESHOLD - MIN_DOWN; LEVELS],
            low: [MIN_DOWN; LEVELS],
            duty: [duty_for(fan, rpm); LEVELS],
            target: [rpm_to_tach(rpm); LEVELS],
        }
    }

    /// Level k+1 starts at point k's temperature and runs at its speed.
    /// Unused levels get unreachable thresholds and repeat the last speed.
    pub fn curve(fan: usize, points: &[CurvePoint], hysteresis: u8) -> Self {
        let mut pts: Vec<CurvePoint> = points.iter().take(LEVELS).copied().collect();
        pts.sort_by_key(|p| p.temp);
        let mut t = Self { up: [255; LEVELS], hyst: [10; LEVELS], low: [255; LEVELS], ..Self::default() };
        let last = pts.last().map_or(0, |p| clamp_rpm(fan, p.rpm));
        for i in 0..LEVELS {
            let rpm = pts.get(i).map_or(last, |p| clamp_rpm(fan, p.rpm));
            if let Some(p) = pts.get(i) {
                let up = p.temp.max(MIN_THRESHOLD);
                t.up[i] = up;
                t.hyst[i] = hysteresis.min(up - MIN_DOWN);
                t.low[i] = up - t.hyst[i];
            }
            t.duty[i] = duty_for(fan, rpm);
            t.target[i] = rpm_to_tach(rpm);
        }
        t
    }
}

pub fn clamp_rpm(fan: usize, rpm: u32) -> u32 {
    if rpm == 0 { 0 } else { rpm.clamp(MIN_RPM, MAX_RPM[fan]) }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp: u8,
    pub rpm: u32,
}

/// How the daemon drives one fan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum FanMode {
    /// Firmware tables from the current platform profile.
    #[default]
    Stock,
    /// One speed regardless of temperature; 0 = off.
    Fixed { rpm: u32 },
    /// Up to 13 (temperature, rpm) points.
    Curve { points: Vec<CurvePoint>, hysteresis: u8 },
    /// Raw table edits; the daemon leaves the rows alone.
    Manual,
}

impl FanMode {
    /// The table to enforce, if the daemon owns this fan's rows.
    pub fn table(&self, fan: usize) -> Option<FanTable> {
        match self {
            Self::Stock | Self::Manual => None,
            Self::Fixed { rpm } => Some(FanTable::fixed(fan, *rpm)),
            Self::Curve { points, hysteresis } => Some(FanTable::curve(fan, points, *hysteresis)),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Stock => "stock".into(),
            Self::Fixed { rpm: 0 } => "off".into(),
            Self::Fixed { rpm } => format!("fixed {rpm} rpm"),
            Self::Curve { points, .. } => format!("curve ({} points)", points.len()),
            Self::Manual => "manual".into(),
        }
    }
}

/// A sensible starting curve: quiet until 55 °C, full by 90 °C.
pub fn default_curve(fan: usize) -> Vec<CurvePoint> {
    let max = MAX_RPM[fan] - 200;
    [(45, 1200), (55, 1400), (65, 1700), (72, 2000), (80, 2300), (90, max)]
        .iter()
        .map(|&(temp, rpm)| CurvePoint { temp, rpm: rpm.min(max) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut block = vec![0u8; 256];
        let t = FanTable::curve(1, &default_curve(1), 4);
        for (addr, v) in t.all_bytes(1) {
            block[(addr - 0x0A00) as usize] = v;
        }
        assert_eq!(FanTable::decode(&block, 1).unwrap(), t);
        assert_eq!(t.up[0], 45);
        assert_eq!((t.hyst[0], t.low[0]), (4, 41));
        assert_eq!(t.up[6], 255);
        assert!(t.up.iter().chain(&t.low).all(|&v| v >= MIN_DOWN));
        let f = FanTable::fixed(0, 1600);
        assert!(f.up.iter().chain(&f.low).all(|&v| v >= MIN_DOWN));
        assert_eq!(t.level_rpm(1), Some(tach_to_rpm(rpm_to_tach(1200))));
    }

    #[test]
    fn owners() {
        assert_eq!(owner(0x0A00), Some(0));
        assert_eq!(owner(0x0A45), Some(1));
        assert_eq!(owner(0x0AD9), Some(0));
        assert_eq!(owner(0x0AE0), Some(1));
        assert_eq!(owner(0x0A90), None);
    }
}
