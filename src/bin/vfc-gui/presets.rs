//! Built-in and user presets (~/.config/vfc/presets.json).

use std::fs;
use std::path::PathBuf;

use vfc::modes::Mode;
use vfc::proto::Control;

fn builtins() -> Vec<(String, Control)> {
    vec![
        ("Stock (firmware)".into(), Mode::Normal.control()),
        ("Silent".into(), Mode::Quiet.control()),
        ("Balanced".into(), balanced()),
        ("Cool".into(), Mode::Cool.control()),
        ("Full speed".into(), Mode::Max.control()),
    ]
}

fn balanced() -> Control {
    use vfc::tables::{CurvePoint, FanMode, MAX_RPM};
    let points = [(50, 1200), (60, 1500), (68, 1800), (75, 2100), (82, 2400), (88, 3200)];
    Control {
        fans: std::array::from_fn(|fan| FanMode::Curve {
            points: points.iter().map(|&(temp, rpm)| CurvePoint { temp, rpm: rpm.min(MAX_RPM[fan]) }).collect(),
            hysteresis: 4,
        }),
    }
}

pub struct Presets {
    builtin: Vec<(String, Control)>,
    user: Vec<(String, Control)>,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vfc/presets.json"))
}

impl Presets {
    pub fn load() -> Self {
        let user = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { builtin: builtins(), user }
    }

    fn store(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.user) {
            let _ = fs::write(p, s);
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.builtin
            .iter()
            .map(|(n, _)| n.clone())
            .chain(self.user.iter().map(|(n, _)| format!("★ {n}")))
            .collect()
    }

    pub fn get(&self, i: usize) -> Option<&Control> {
        self.builtin.iter().chain(&self.user).nth(i).map(|(_, c)| c)
    }

    pub fn is_user(&self, i: usize) -> bool {
        i >= self.builtin.len() && i < self.builtin.len() + self.user.len()
    }

    pub fn delete(&mut self, i: usize) {
        if self.is_user(i) {
            self.user.remove(i - self.builtin.len());
            self.store();
        }
    }

    pub fn save(&mut self, name: &str, c: Control) {
        match self.user.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = c,
            None => self.user.push((name.to_owned(), c)),
        }
        self.store();
    }
}
