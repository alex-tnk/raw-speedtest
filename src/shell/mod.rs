//! The native UI for each platform. The core (engine, app, view) is shared;
//! each shell lays out the platform's own controls and draws the core's
//! chart scenes with the platform's own graphics API.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Runs the UI until the app quits.
pub fn run() {
    // `RST_EGUI` keeps the previous egui UI reachable while the native
    // shells are brought to parity.
    if std::env::var_os("RST_EGUI").is_some() {
        return crate::ui::run();
    }
    #[cfg(target_os = "linux")]
    linux::run();
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(target_os = "windows")]
    windows::run();
}
