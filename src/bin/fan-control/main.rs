//! fan-control — the simple app: a temperature and four big buttons.
//! Everything else lives in vfc-gui.

#[path = "../vfc-gui/backend.rs"]
#[allow(dead_code)]
mod backend;

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, Ui, text::LayoutJob};

use backend::Backend;
use vfc::modes::{self, Mode};
use vfc::proto::{Request, Status};

const GOOD: Color32 = Color32::from_rgb(110, 200, 120);
const WARN: Color32 = Color32::from_rgb(240, 190, 60);
const BAD: Color32 = Color32::from_rgb(235, 80, 80);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Fan Control")
            .with_app_id("fan-control")
            .with_inner_size([440.0, 640.0])
            .with_min_inner_size([380.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("fan-control", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

struct App {
    backend: Backend,
    /// The mode just clicked, until the daemon reports it.
    pending: Option<(Mode, Instant)>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        ctx.set_zoom_factor(1.2);
        Self { backend: Backend::start(ctx.clone()), pending: None }
    }

    fn pick(&mut self, mode: Mode) {
        let c = mode.control();
        self.backend.send(Request::SetControl(c.clone()), mode.name());
        // Remembered after a restart too.
        self.backend.send(Request::SetBoot(c), mode.name());
        self.pending = Some((mode, Instant::now()));
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
            ui.add_space(12.0);
            match status.filter(|_| connected) {
                Some(st) => self.main_view(ui, &st, failed),
                None => not_running(ui),
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

impl App {
    fn main_view(&mut self, ui: &mut Ui, st: &Status, failed: bool) {
        let current = Mode::of(&st.control);
        if self.pending.is_some_and(|(m, at)| Some(m) == current || at.elapsed() > Duration::from_secs(15)) {
            self.pending = None;
        }

        let temp = st.cpu_temp.unwrap_or(f32::from(st.fans[0].temp));
        let (color, words) = match temp {
            t if t < 60.0 => (GOOD, "Cool. All good."),
            t if t < 80.0 => (GOOD, "Warm. That's normal."),
            t if t < 90.0 => (WARN, "Hot."),
            _ => (BAD, "Very hot!"),
        };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(format!("{temp:.0} °C")).size(56.0).strong().color(color));
            ui.label(RichText::new(words).size(20.0).color(color));
            ui.add_space(4.0);
            let rpm = st.fans[0].rpm.max(st.fans[1].rpm);
            let fans = match rpm {
                0 => "Fans are off",
                1..1600 => "Fans are slow",
                1600..2400 => "Fans are medium",
                _ => "Fans are fast",
            };
            ui.label(RichText::new(fans).size(16.0).weak());
            if st.guard_active {
                ui.add_space(6.0);
                ui.label(RichText::new("Too hot: fans are on full until it cools down.").size(15.0).color(BAD));
            }
        });

        ui.add_space(20.0);
        let selected = self.pending.map(|(m, _)| m).or(current);
        for mode in modes::ALL {
            let mut job = LayoutJob::default();
            let text = ui.visuals().strong_text_color();
            job.append(mode.name(), 0.0, egui::TextFormat::simple(FontId::proportional(22.0), text));
            job.append("\n", 0.0, egui::TextFormat::default());
            job.append(mode.description(), 0.0, egui::TextFormat::simple(FontId::proportional(15.0), ui.visuals().text_color()));
            let button = egui::Button::new(job)
                .selected(selected == Some(mode))
                .min_size(egui::vec2(ui.available_width(), 70.0));
            if ui.add(button).clicked() && selected != Some(mode) {
                self.pick(mode);
            }
            ui.add_space(8.0);
        }

        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            if failed {
                ui.label(RichText::new("That didn't work. Please try again.").color(BAD));
            } else if self.pending.is_some() {
                ui.label(RichText::new("Switching…").weak());
            } else if current.is_none() {
                ui.label(RichText::new("Custom settings are on. Pick one above to go back.").weak());
            } else {
                ui.label(RichText::new("Your choice stays on after a restart.").weak());
            }
        });
    }
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
