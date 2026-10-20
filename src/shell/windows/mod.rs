//! Windows shell: Win32 controls (common controls v6), Direct2D and
//! DirectWrite charts, a Mica title bar, and the system light or dark
//! appearance. One standalone executable; nothing is written to disk.

mod app;
mod chart;
mod gfx;
mod log;
mod settings;
mod text;
mod theme;
mod tray;
mod win;

pub fn run() {
    app::run();
}
