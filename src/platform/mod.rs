//! Everything OS-specific lives behind `Platform`. Each OS module implements
//! it; values a platform can't provide come back as `None` and the UI shows
//! them as unavailable. `#[cfg(target_os)]` appears only in this module tree.

use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::model::{InterfaceInfo, TcpStats, WifiInfo};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_tray;
#[cfg(unix)]
mod unix;
#[cfg(target_os = "windows")]
mod windows;

/// The system UI font.
pub struct UiFont {
    pub data: Vec<u8>,
    /// Font with a `wght` (and maybe `opsz`) axis; semibold is a variation.
    pub variable: bool,
    /// Separate semibold face for platforms whose UI font is static.
    pub semibold: Option<Vec<u8>>,
    pub has_opsz: bool,
    pub source: String,
}

#[derive(Debug)]
// Only the Unix ping constructs Timeout and Failed so far; the Windows
// implementation reports Unsupported.
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub enum PingError {
    Timeout,
    Unsupported,
    Failed(String),
}

/// What the menu bar item shows. The UI formats it; the platform only
/// displays it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayStatus {
    /// Compact value next to the icon (latency); empty shows the icon only.
    pub title: String,
    /// `(label, value)` rows: download, upload, latency, jitter, loss.
    pub rows: Vec<(String, String)>,
    /// The status line, as in the window.
    pub status: String,
    pub one_shot_running: bool,
    pub continuous_running: bool,
    pub paused: bool,
}

/// A command picked from the menu bar item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Only the macOS menu bar item creates these; elsewhere the UI's handling of
// them is never reached.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum TrayAction {
    RunOnce,
    RunContinuously,
    PauseResume,
    Stop,
    ShowWindow,
    Quit,
}

/// Asks the UI loop to run soon (to act on a menu bar pick).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub trait Platform: Send + Sync {
    /// Kernel TCP counters for a connected socket.
    fn tcp_stats(&self, sock: &TcpStream) -> Option<TcpStats>;

    /// Interface that owns `local`, with its type, MTU and default gateway.
    fn interface_for(&self, local: IpAddr) -> InterfaceInfo;

    /// Current Wi-Fi association, `None` without Wi-Fi support.
    fn wifi(&self, interface: Option<&str>) -> Option<WifiInfo>;

    /// One ICMP echo round trip.
    fn ping(&self, target: IpAddr, seq: u16, timeout: Duration) -> Result<Duration, PingError>;

    fn ui_font(&self) -> Option<UiFont>;

    /// Local time zone offset from UTC in seconds at `unix_secs`, `None`
    /// when unknown (times are then shown in UTC).
    fn utc_offset(&self, unix_secs: i64) -> Option<i32>;

    /// Called once before the window exists: turn off OS features that
    /// would write state to disk.
    fn prepare_process(&self) {}

    /// Called once the main window exists.
    fn window_created(&self) {}

    /// Whether the app can live in a menu bar (or tray) item.
    fn tray_supported(&self) -> bool {
        false
    }

    /// Show or remove the menu bar item. Main thread.
    fn tray_set(&self, _on: bool, _wake: Wake) {}

    /// Refresh what the menu bar item shows. Main thread.
    fn tray_update(&self, _status: &TrayStatus) {}

    /// Menu bar picks since the last call.
    fn tray_take_actions(&self) -> Vec<TrayAction> {
        Vec::new()
    }

    /// Show or hide the Dock icon, for running with the window closed.
    fn set_dock_icon(&self, _visible: bool) {}

    /// Bring the app to the front.
    fn activate(&self) {}

    /// Quit through the platform's normal path so exit cleanup runs.
    fn quit(&self) {
        std::process::exit(0);
    }

    /// Ask the user where to save an export. Blocks until answered.
    fn choose_save_path(&self, suggested_name: &str) -> Result<Option<PathBuf>, String>;
}

pub fn current() -> &'static dyn Platform {
    #[cfg(target_os = "macos")]
    {
        &macos::MacOs
    }
    #[cfg(target_os = "windows")]
    {
        &windows::Windows
    }
    #[cfg(target_os = "linux")]
    {
        &linux::Linux
    }
}
