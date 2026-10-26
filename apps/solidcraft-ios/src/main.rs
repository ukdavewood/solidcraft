//! SolidCraft on iPad (and iPhone).
//!
//! Runs the same [`solidcraft_ui_egui::SolveApp`] as the desktop app through eframe's native
//! runner; winit's iOS backend supplies the UIKit window and touch/Pencil events, wgpu draws with
//! Metal. Stage 1: the app starts with the sample part; Files-app integration, Pencil gestures and
//! scaling follow.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod files;
#[cfg(target_os = "ios")]
mod ai;
#[cfg(any(target_os = "ios", test))]
mod ai_data;
#[cfg(target_os = "ios")]
mod keyboard;
#[cfg(target_os = "ios")]
mod picker;

use solidcraft_engine::Session;
use solidcraft_ui_egui::SolveApp;

/// A 32-bit float depth buffer, as on the other apps (reversed-depth viewport).
const DEPTH_BITS: u8 = 32;

struct App {
    solve: SolveApp,
    files: files::Files,
    #[cfg(target_os = "ios")]
    ai: ai::State,
    frames: u32,
    synced_at: f64,
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.frames < 4 {
            self.frames += 1;
            let (insets, screen, content) = ctx.input(|i| (i.safe_area_insets(), i.viewport_rect(), i.content_rect()));
            note(&format!("frame {}: insets {insets:?} screen {screen:?} content {content:?}", self.frames));
        }
        // Test hook: `SIMCTL_CHILD_SOLIDCRAFT_IOS_TEST=doc.open` starts that command after start-up.
        if self.frames == 3 {
            self.frames += 1;
            if let Ok(id) = std::env::var("SOLIDCRAFT_IOS_TEST") {
                self.solve.start(&id);
            }
        }
        // Saves of a file opened from iCloud Drive (or another folder) are written back to it.
        #[cfg(target_os = "ios")]
        if picker::linked() {
            let now = ctx.input(|i| i.time);
            if now - self.synced_at >= 1.0 {
                self.synced_at = now;
                for (text, error) in picker::sync() {
                    self.solve.set_status(text, error);
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        #[cfg(target_os = "ios")]
        self.ai.tick(&mut self.solve);
        self.solve.logic(ctx);
    }
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        // The software keyboard's Return, as an Enter key press and release.
        #[cfg(target_os = "ios")]
        for _ in 0..keyboard::take_returns() {
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        self.solve.raw_input_hook(raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Inside the status bar, the home indicator and any rounded corners.
        #[allow(unused_mut)]
        let mut safe = ui.ctx().content_rect();
        // A docked keyboard covers the bottom: the UI sits above it, so the field being typed in
        // is never hidden (a floating or split keyboard leaves the layout alone).
        #[cfg(target_os = "ios")]
        {
            keyboard::install(ui.ctx());
            let covered = (keyboard::covered() - ui.ctx().input(|i| i.safe_area_insets().0.bottom)).max(0.0);
            safe.max.y = (safe.max.y - covered).max(safe.min.y + 200.0);
        }
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(safe));
        inner.set_clip_rect(safe);
        self.solve.ui(&mut inner);
        // Number fields ask for the number keyboard.
        #[cfg(target_os = "ios")]
        keyboard::set_numeric(solidcraft_ui_egui::numeric_focus(ui.ctx()));
        self.files.show(ui.ctx());
        #[cfg(target_os = "ios")]
        // The AI window rides above a docked keyboard.
        #[cfg(target_os = "ios")]
        {
            let lift = (keyboard::covered() - ui.ctx().input(|i| i.safe_area_insets().0.bottom)).max(0.0);
            self.ai.show(ui.ctx(), lift);
        }
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(solidcraft_ui_egui::prefs::STORAGE_KEY, self.solve.prefs());
    }
}

/// Where a failure is written: the simulator and device show no console, so a start that ends
/// at once leaves its reason here (the app's tmp folder).
fn note(text: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("solidcraft-last-error.txt")) {
        let _ = writeln!(f, "{text}");
    }
}

/// Sends `log` records (eframe, winit and wgpu report start-up failures there) to the same file.
struct FileLog;

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            note(&format!("{} {}: {}", r.level(), r.target(), r.args()));
        }
    }
    fn flush(&self) {}
}

fn main() -> eframe::Result {
    if log::set_logger(&FileLog).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    std::panic::set_hook(Box::new(|info| note(&format!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture()))));
    note("start");
    let result = run();
    match &result {
        Ok(()) => note("run_native returned Ok"),
        Err(e) => note(&format!("exit: {e}")),
    }
    result
}

fn run() -> eframe::Result {
    // The simulator's GPU (and some devices) offers less than wgpu's default limits (15 stage
    // variables, not 16): ask for the lesser of the two.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        let base = setup.device_descriptor.clone();
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            d.required_limits = d.required_limits.or_worse_values_from(&adapter.limits());
            d
        });
    }
    // Settings live in the app's Library folder (the platform has no data directory std can find).
    let library = std::env::temp_dir().parent().map(|p| p.join("Library"));
    if let Some(dir) = &library {
        let _ = std::fs::create_dir_all(dir);
    }
    let options = eframe::NativeOptions {
        depth_buffer: DEPTH_BITS,
        wgpu_options,
        persistence_path: library.map(|d| d.join("app.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "SolidCraft",
        options,
        Box::new(|cc| {
            note("creating app");
            solidcraft_ui_egui::fonts::setup_fonts(&cc.egui_ctx);
            let files = files::Files::default();
            let mut app = SolveApp::new(Session::default(), files.services());
            if let Some(prefs) = cc.storage.and_then(|s| solidcraft_ui_egui::prefs::stored(|k| s.get_string(k))) {
                app.load_prefs(&prefs);
            }
            if let Some(rs) = &cc.wgpu_render_state {
                app.set_wgpu(rs, DEPTH_BITS, 0, rs.msaa_samples);
                solidcraft_ui_egui::gpu::install_picture_renderer(rs);
            }
            if let Err(e) = app.session.run_script(&solidcraft_engine::sample::script()) {
                app.set_status(format!("sample: {e}"), true);
            }
            note("app created");
            Ok(Box::new(App {
                solve: app,
                files,
                #[cfg(target_os = "ios")]
                ai: ai::State::default(),
                frames: 0,
                synced_at: 0.0,
            }))
        }),
    )
}
