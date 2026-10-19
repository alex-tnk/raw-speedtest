//! Raw Speed Test: a fine-grained internet speed test against Cloudflare's
//! public speed test endpoints.

mod app;
mod engine;
mod model;
mod net;
mod platform;
mod stats;
mod ui;

fn main() {
    platform::current().prepare_process();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Raw Speed Test")
            .with_app_id("raw-speed-test")
            .with_inner_size(ui::theme::WINDOW_DEFAULT)
            .with_min_inner_size(ui::theme::WINDOW_MIN),
        centered: true,
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        system_font_fallback: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Raw Speed Test",
        options,
        Box::new(|cc| match ui::SpeedApp::new(cc) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    );
    if let Err(e) = result {
        eprintln!("raw-speed-test: {e}");
        std::process::exit(1);
    }
}
