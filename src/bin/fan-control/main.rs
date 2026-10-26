//! fan-control — the simple app: a temperature, four big buttons, and
//! "my speed" / "my curve" for people who want a number of their own.
//! Everything else lives in vfc-gui.

#[path = "../vfc-gui/backend.rs"]
#[allow(dead_code)]
mod backend;

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, Ui, text::LayoutJob};

use backend::Backend;
use vfc::modes::{self, Mode};
use vfc::proto::{Control, Request, Status};
use vfc::tables::{CurvePoint, FanMode, MAX_RPM, MIN_RPM, clamp_rpm};

const GOOD: Color32 = Color32::from_rgb(110, 200, 120);
const WARN: Color32 = Color32::from_rgb(240, 190, 60);
const BAD: Color32 = Color32::from_rgb(235, 80, 80);

/// Allowed range for the user's own numbers. Below MIN_RPM the fans may
/// stall; above 90 °C the guard (95 °C) would be the only thing left.
const TOP_RPM: u32 = MAX_RPM[1];
const LOW_TEMP: u8 = 30;
const HIGH_TEMP: u8 = 90;
const MAX_ROWS: usize = 8;
const HYSTERESIS: u8 = 4;
/// A fan counts as "at speed" within this fraction of its target.
const TOLERANCE: f32 = 0.10;
/// The EC slews ~30 rpm/s, so a full-range change takes ~90 s.
const SPIN_UP: Duration = Duration::from_secs(120);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Fan Control")
            .with_app_id("fan-control")
            .with_inner_size([460.0, 820.0])
            .with_min_inner_size([400.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("fan-control", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Preset(Mode),
    Speed,
    Curve,
}

impl Choice {
    fn of(c: &Control) -> Option<Self> {
        if let Some(m) = Mode::of(c) {
            return Some(Self::Preset(m));
        }
        match &c.fans[0] {
            FanMode::Fixed { rpm } if *rpm > 0 => Some(Self::Speed),
            FanMode::Curve { .. } => Some(Self::Curve),
            _ => None,
        }
    }
}

/// Something just sent, until the daemon confirms it.
struct Sent {
    control: Control,
    choice: Choice,
    at: Instant,
}

struct App {
    backend: Backend,
    sent: Option<Sent>,
    /// Which of "my speed" / "my curve" is unfolded.
    open: Option<Choice>,
    /// Bring the form into view on the next frame.
    scroll: u8,
    speed: String,
    rows: Vec<[String; 2]>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        ctx.set_zoom_factor(1.2);
        Self {
            backend: Backend::start(ctx.clone()),
            sent: None,
            open: match std::env::args().nth(1).as_deref() {
                Some("speed") => Some(Choice::Speed),
                Some("curve") => Some(Choice::Curve),
                _ => None,
            },
            scroll: 5,
            speed: "1800".into(),
            rows: [(50, 1200), (65, 1800), (80, 2600), (90, 3200)]
                .iter()
                .map(|(t, r)| [t.to_string(), r.to_string()])
                .collect(),
        }
    }

    fn apply(&mut self, control: Control, choice: Choice) {
        self.backend.send(Request::SetControl(control.clone()), "apply");
        // Remembered after a restart too.
        self.backend.send(Request::SetBoot(control.clone()), "remember");
        self.sent = Some(Sent { control, choice, at: Instant::now() });
    }

    /// Fill the forms from what is running, when it is the user's own.
    fn load(&mut self, c: &Control) {
        match &c.fans[..] {
            [FanMode::Fixed { rpm: a }, FanMode::Fixed { rpm: b }] if *a > 0 => {
                self.speed = (*a).max(*b).to_string();
            }
            [FanMode::Curve { .. }, FanMode::Curve { points, .. }] if Mode::of(c).is_none() => {
                self.rows = points.iter().map(|p| [p.temp.to_string(), p.rpm.to_string()]).collect();
            }
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let (status, connected, failed) = {
            let mut s = self.backend.state();
            let failed = s.notices.iter().any(|n| !n.ok);
            s.notices.clear();
            (s.status.clone(), s.connected, failed)
        };
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.add_space(12.0);
                match status.filter(|_| connected) {
                    Some(st) => self.main_view(ui, &st, failed),
                    None => not_running(ui),
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

fn big_button(ui: &mut Ui, title: &str, text: &str, selected: bool, width: f32) -> bool {
    let mut job = LayoutJob::default();
    job.append(title, 0.0, egui::TextFormat::simple(FontId::proportional(22.0), ui.visuals().strong_text_color()));
    job.append("\n", 0.0, egui::TextFormat::default());
    job.append(text, 0.0, egui::TextFormat::simple(FontId::proportional(15.0), ui.visuals().text_color()));
    let button = egui::Button::new(job).selected(selected).min_size(egui::vec2(width, 62.0));
    ui.add(button).clicked()
}

fn current_temp(st: &Status) -> f32 {
    st.cpu_temp.unwrap_or(f32::from(st.fans[0].temp))
}

impl App {
    fn main_view(&mut self, ui: &mut Ui, st: &Status, failed: bool) {
        let current = Choice::of(&st.control);
        if self.sent.as_ref().is_some_and(|s| s.at.elapsed() > SPIN_UP + Duration::from_secs(20)) {
            self.sent = None;
        }

        let temp = current_temp(st);
        let (color, words) = match temp {
            t if t < 60.0 => (GOOD, "Cool. All good."),
            t if t < 80.0 => (GOOD, "Warm. That's normal."),
            t if t < 90.0 => (WARN, "Hot."),
            _ => (BAD, "Very hot!"),
        };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(format!("{temp:.0} °C")).size(48.0).strong().color(color));
            ui.label(RichText::new(words).size(20.0).color(color));
            ui.add_space(4.0);
            let fans = match st.fans[0].rpm.max(st.fans[1].rpm) {
                0 => "Fans are off".to_owned(),
                _ => format!("Fans: {} and {} rpm", st.fans[0].rpm, st.fans[1].rpm),
            };
            ui.label(RichText::new(fans).size(16.0).weak());
            if st.guard_active {
                ui.add_space(6.0);
                ui.label(RichText::new("Too hot: fans are on full until it cools down.").size(15.0).color(BAD));
            }
        });

        ui.add_space(20.0);
        let selected = self.sent.as_ref().map(|s| s.choice).or(current);
        for mode in modes::ALL {
            let chosen = selected == Some(Choice::Preset(mode));
            if big_button(ui, mode.name(), mode.description(), chosen, ui.available_width()) && !chosen {
                self.open = None;
                self.apply(mode.control(), Choice::Preset(mode));
            }
            ui.add_space(6.0);
        }

        ui.add_space(4.0);
        ui.label(RichText::new("Or set your own:").size(16.0).weak());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            for (choice, title, text) in [
                (Choice::Speed, "My speed", "One speed, always."),
                (Choice::Curve, "My curve", "Speed by temperature."),
            ] {
                let mut title = title.to_owned();
                if self.open == Some(choice) {
                    title.push_str("  ▴");
                } else {
                    title.push_str("  ▾");
                }
                if big_button(ui, &title, text, selected == Some(choice), half) {
                    if self.open == Some(choice) {
                        self.open = None;
                    } else {
                        self.load(&st.control);
                        self.open = Some(choice);
                        self.scroll = 5;
                    }
                }
            }
        });
        if let Some(choice) = self.open {
            ui.add_space(8.0);
            let form = egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                match choice {
                    Choice::Speed => self.speed_form(ui),
                    _ => self.curve_form(ui, st),
                }
            });
            if self.scroll > 0 {
                self.scroll -= 1;
                form.response.scroll_to_me(Some(egui::Align::BOTTOM));
                ui.ctx().request_repaint();
            }
        }

        ui.add_space(6.0);
        let footer = ui.vertical_centered(|ui| self.footer(ui, st, current, failed));
        if self.sent.as_ref().is_some_and(|s| s.at.elapsed() < Duration::from_secs(1)) {
            footer.response.scroll_to_me(None);
        }
        ui.add_space(12.0);
    }

    fn speed_form(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Speed:").size(18.0));
            ui.add(egui::TextEdit::singleline(&mut self.speed).desired_width(90.0).font(FontId::proportional(20.0)));
            ui.label(RichText::new("rpm").size(18.0));
        });
        ui.label(RichText::new(format!("Allowed: {MIN_RPM} to {TOP_RPM}. About 1500 is quiet, 2500 is loud.")).weak());
        ui.add_space(6.0);
        match check_speed(&self.speed) {
            Err(e) => {
                ui.label(RichText::new(e).color(BAD));
            }
            Ok(rpm) => {
                if rpm > MAX_RPM[0] {
                    ui.label(
                        RichText::new(format!("Fan 1 can only do {} rpm, so it will run at that.", MAX_RPM[0])).weak(),
                    );
                }
                let set = egui::Button::new(RichText::new(format!("Set {rpm} rpm")).size(18.0));
                if ui.add(set.min_size(egui::vec2(160.0, 40.0))).clicked() {
                    let control = Control { fans: std::array::from_fn(|f| FanMode::Fixed { rpm: clamp_rpm(f, rpm) }) };
                    self.apply(control, Choice::Speed);
                }
            }
        }
    }

    fn curve_form(&mut self, ui: &mut Ui, st: &Status) {
        ui.label(RichText::new("When it gets this hot, run the fans this fast:").size(16.0));
        ui.add_space(4.0);
        let mut remove = None;
        for (i, row) in self.rows.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("At").size(17.0));
                ui.add_sized([52.0, 30.0], egui::TextEdit::singleline(&mut row[0]).font(FontId::proportional(18.0)));
                ui.label(RichText::new("°C  →").size(17.0));
                ui.add_sized([72.0, 30.0], egui::TextEdit::singleline(&mut row[1]).font(FontId::proportional(18.0)));
                ui.label(RichText::new("rpm").size(17.0));
                if ui.small_button("✖").on_hover_text("Remove this line").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove.filter(|_| self.rows.len() > 1) {
            self.rows.remove(i);
        }
        if self.rows.len() < MAX_ROWS && ui.button("+ Add a line").clicked() {
            let last = self.rows.last().cloned().unwrap_or_else(|| ["60".into(), "1500".into()]);
            let t = last[0].trim().parse::<u8>().map_or(60, |t| t.saturating_add(5).min(HIGH_TEMP));
            self.rows.push([t.to_string(), last[1].clone()]);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "Temperatures {LOW_TEMP}–{HIGH_TEMP} °C, speeds {MIN_RPM}–{TOP_RPM} rpm. \
                 Below the first temperature the fans are off."
            ))
            .weak(),
        );
        ui.add_space(6.0);
        match check_curve(&self.rows) {
            Err(e) => {
                ui.label(RichText::new(e).color(BAD));
            }
            Ok(points) => {
                let now = current_temp(st);
                let here = points.iter().rev().find(|p| f32::from(p.temp) <= now).map(|p| p.rpm);
                let means = here.map_or("fans off".to_owned(), |r| format!("{r} rpm"));
                ui.label(RichText::new(format!("Right now ({now:.0} °C) this means: {means}")).size(15.0));
                let set = egui::Button::new(RichText::new("Use this curve").size(18.0));
                if ui.add(set.min_size(egui::vec2(160.0, 40.0))).clicked() {
                    let control = Control {
                        fans: std::array::from_fn(|f| FanMode::Curve {
                            points: points
                                .iter()
                                .map(|p| CurvePoint { temp: p.temp, rpm: clamp_rpm(f, p.rpm) })
                                .collect(),
                            hysteresis: HYSTERESIS,
                        }),
                    };
                    self.apply(control, Choice::Curve);
                }
            }
        }
    }

    /// One line saying what just happened, checked against the real fans.
    fn footer(&mut self, ui: &mut Ui, st: &Status, current: Option<Choice>, failed: bool) {
        let say = |ui: &mut Ui, text: &str, color: Option<Color32>| {
            let t = RichText::new(text).size(16.0);
            ui.label(match color {
                Some(c) => t.color(c),
                None => t.weak(),
            });
        };
        if failed {
            self.sent = None;
            return say(ui, "That didn't work. Please try again.", Some(BAD));
        }
        let Some(sent) = &self.sent else {
            return match current {
                None => say(ui, "Custom settings are on. Pick one above to go back.", None),
                Some(_) => say(ui, "Your choice stays on after a restart.", None),
            };
        };
        if st.control != sent.control {
            return say(ui, "Switching…", None);
        }
        // Speeds are only promised for a fixed speed; presets and curves
        // follow the temperature.
        let FanMode::Fixed { .. } = sent.control.fans[0] else {
            return say(ui, "✔ Done. Fans may take a minute to settle.", Some(GOOD));
        };
        if st.guard_active {
            return say(ui, "Too hot right now, so the fans are on full.", Some(WARN));
        }
        let at_speed = (0..2).all(|f| {
            let FanMode::Fixed { rpm } = sent.control.fans[f] else { return false };
            (st.fans[f].rpm as f32 - rpm as f32).abs() <= rpm as f32 * TOLERANCE
        });
        if at_speed {
            let text = format!("✔ Checked: fans at {} and {} rpm.", st.fans[0].rpm, st.fans[1].rpm);
            say(ui, &text, Some(GOOD));
        } else if sent.at.elapsed() < SPIN_UP {
            let text = format!("Fans are changing speed… now {} and {} rpm", st.fans[0].rpm, st.fans[1].rpm);
            say(ui, &text, None);
        } else {
            let text =
                format!("⚠ Fans only reached {} and {} rpm. Try another number.", st.fans[0].rpm, st.fans[1].rpm);
            say(ui, &text, Some(WARN));
        }
    }
}

fn check_speed(text: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Type a number.".into());
    }
    let rpm: u32 = text.parse().map_err(|_| "Only digits, please (like 1800).".to_owned())?;
    if !(MIN_RPM..=TOP_RPM).contains(&rpm) {
        return Err(format!("Pick a number from {MIN_RPM} to {TOP_RPM}."));
    }
    Ok(rpm)
}

fn check_curve(rows: &[[String; 2]]) -> Result<Vec<CurvePoint>, String> {
    let mut points: Vec<CurvePoint> = Vec::new();
    for (i, [t, r]) in rows.iter().enumerate() {
        let line = i + 1;
        let temp: u8 = t.trim().parse().map_err(|_| format!("Line {line}: type a temperature (like 60)."))?;
        if !(LOW_TEMP..=HIGH_TEMP).contains(&temp) {
            return Err(format!("Line {line}: temperature must be {LOW_TEMP} to {HIGH_TEMP} °C."));
        }
        let rpm = check_speed(r).map_err(|e| format!("Line {line}: {e}"))?;
        if let Some(prev) = points.last() {
            if temp <= prev.temp {
                return Err(format!("Line {line}: each temperature must be higher than the line above."));
            }
            if rpm < prev.rpm {
                return Err(format!("Line {line}: the fans can't get slower when it gets hotter."));
            }
        }
        points.push(CurvePoint { temp, rpm });
    }
    if points.is_empty() {
        return Err("Add at least one line.".into());
    }
    Ok(points)
}

fn not_running(ui: &mut Ui) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Fan control isn't running").size(24.0).strong());
        ui.add_space(10.0);
        ui.label(RichText::new("Press the button below. It will ask for your password.").size(16.0));
        ui.add_space(16.0);
        let fix = egui::Button::new(RichText::new("Fix it").size(22.0)).min_size(egui::vec2(200.0, 56.0));
        if ui.add(fix).clicked() {
            let _ = std::process::Command::new("pkexec").args(["systemctl", "restart", "vfcd"]).spawn();
        }
        ui.add_space(16.0);
        ui.label(RichText::new("If that doesn't help, restart the computer.").weak());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[(&str, &str)]) -> Vec<[String; 2]> {
        r.iter().map(|(a, b)| [a.to_string(), b.to_string()]).collect()
    }

    #[test]
    fn speed_checks() {
        assert_eq!(check_speed(" 1800 "), Ok(1800));
        assert!(check_speed("").is_err());
        assert!(check_speed("abc").is_err());
        assert!(check_speed("-5").is_err());
        assert!(check_speed("200").is_err());
        assert!(check_speed("9000").is_err());
        assert!(check_speed("99999999999").is_err());
    }

    #[test]
    fn curve_checks() {
        assert!(check_curve(&rows(&[("50", "1200"), ("70", "2000")])).is_ok());
        assert!(check_curve(&[]).is_err());
        assert!(check_curve(&rows(&[("20", "1200")])).is_err(), "too cold");
        assert!(check_curve(&rows(&[("95", "1200")])).is_err(), "too hot");
        assert!(check_curve(&rows(&[("60", "1200"), ("60", "1500")])).is_err(), "same temp");
        assert!(check_curve(&rows(&[("60", "2000"), ("70", "1500")])).is_err(), "slower when hotter");
        assert!(check_curve(&rows(&[("60", "100")])).is_err(), "stall speed");
    }
}
