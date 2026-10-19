use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use super::{PingError, Platform, UiFont};
use crate::model::{InterfaceInfo, TcpStats, WifiInfo};

/// Windows is a compile target only so far: values are reported as
/// unavailable until SIO_TCP_INFO, GetAdaptersAddresses, WLAN API,
/// IcmpSendEcho and IFileSaveDialog are wired up.
pub struct Windows;

impl Platform for Windows {
    fn tcp_stats(&self, _sock: &TcpStream) -> Option<TcpStats> {
        None
    }

    fn interface_for(&self, local: IpAddr) -> InterfaceInfo {
        InterfaceInfo {
            local_ip: Some(local),
            ..InterfaceInfo::default()
        }
    }

    fn wifi(&self, _interface: Option<&str>) -> Option<WifiInfo> {
        None
    }

    fn ping(&self, _target: IpAddr, _seq: u16, _timeout: Duration) -> Result<Duration, PingError> {
        Err(PingError::Unsupported)
    }

    fn utc_offset(&self, _unix_secs: i64) -> Option<i32> {
        None
    }

    fn ui_font(&self) -> Option<UiFont> {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{dir}\Fonts\segoeui.ttf");
        let data = std::fs::read(&path).ok()?;
        let semibold = std::fs::read(format!(r"{dir}\Fonts\seguisb.ttf")).ok();
        Some(UiFont {
            data,
            variable: false,
            semibold,
            has_opsz: false,
            source: path,
        })
    }

    fn choose_save_path(&self, _suggested_name: &str) -> Result<Option<PathBuf>, String> {
        Err("Export needs a save dialog, which is not implemented on Windows yet".into())
    }
}
