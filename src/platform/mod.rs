//! Everything OS-specific lives behind `Platform`. Each OS module implements
//! it; values a platform can't provide come back as `None` and the UI shows
//! them as unavailable. `#[cfg(target_os)]` appears only in this module tree.

use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use crate::model::{InterfaceInfo, TcpStats, WifiInfo};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
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
