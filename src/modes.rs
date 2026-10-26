//! The four everyday fan modes, shared by the simple app and the presets.

use crate::proto::Control;
use crate::tables::{CurvePoint, FanMode, MAX_RPM};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Quiet,
    Normal,
    Cool,
    Max,
}

pub const ALL: [Mode; 4] = [Mode::Quiet, Mode::Normal, Mode::Cool, Mode::Max];

fn curve(points: &[(u8, u32)], hysteresis: u8) -> Control {
    Control {
        fans: std::array::from_fn(|fan| FanMode::Curve {
            points: points.iter().map(|&(temp, rpm)| CurvePoint { temp, rpm: rpm.min(MAX_RPM[fan]) }).collect(),
            hysteresis,
        }),
    }
}

impl Mode {
    pub fn control(self) -> Control {
        match self {
            Self::Quiet => curve(&[(60, 1100), (70, 1400), (78, 1800), (85, 2200), (92, 3200)], 5),
            Self::Normal => Control::default(),
            Self::Cool => curve(&[(0, 1200), (50, 1600), (60, 2000), (70, 2400), (78, 3200)], 3),
            Self::Max => Control { fans: std::array::from_fn(|fan| FanMode::Fixed { rpm: MAX_RPM[fan] }) },
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Quiet => "Quiet",
            Self::Normal => "Normal",
            Self::Cool => "Cool",
            Self::Max => "Max",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Quiet => "Fans stay off or slow. The laptop gets warmer.",
            Self::Normal => "The way the laptop came from the factory.",
            Self::Cool => "Fans always on. The laptop stays cooler.",
            Self::Max => "Fans at full blast. Loud, but coolest.",
        }
    }

    /// The mode this control matches, if any.
    pub fn of(c: &Control) -> Option<Self> {
        ALL.into_iter().find(|m| m.control() == *c)
    }
}
