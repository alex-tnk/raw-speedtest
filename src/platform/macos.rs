use std::ffi::CString;
use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSModalResponseOK, NSSavePanel};
use objc2_core_wlan::{CWInterface, CWWiFiClient};
use objc2_foundation::{NSBundle, NSDictionary, NSNumber, NSProcessInfo, NSString, NSUserDefaults};

use super::macos_tray;
use super::unix::{self, kind_from_name};
use super::{PingError, Platform, UiFont};
use crate::model::{InterfaceInfo, TcpStats, WifiInfo};

pub struct MacOs;

extern "C-unwind" fn refuse(
    _this: &AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    _coder: *mut AnyObject,
) -> Bool {
    Bool::NO
}

/// Remove the empty per-app directories the system creates even though
/// nothing is stored in them: `<user cache dir>/<bundle id>` (graphics
/// stack, shader cache disabled) and `<user temp dir>/<bundle id>.savedState`
/// (AppKit at quit, state saving refused). A directory with any file in it
/// is left alone.
extern "C" fn remove_empty_state_dirs() {
    fn only_empty_dirs(p: &std::path::Path) -> bool {
        match std::fs::read_dir(p) {
            Ok(entries) => entries
                .flatten()
                .all(|e| e.file_type().is_ok_and(|t| t.is_dir()) && only_empty_dirs(&e.path())),
            Err(_) => false,
        }
    }
    fn remove_tree(p: &std::path::Path) {
        if let Ok(entries) = std::fs::read_dir(p) {
            for e in entries.flatten() {
                remove_tree(&e.path());
            }
        }
        let _ = std::fs::remove_dir(p);
    }
    fn darwin_dir(name: libc::c_int) -> Option<std::path::PathBuf> {
        let mut buf = [0u8; 1024];
        // SAFETY: confstr writes at most buf.len() bytes, NUL-terminated.
        let n = unsafe { libc::confstr(name, buf.as_mut_ptr().cast(), buf.len()) };
        (n > 1 && n <= buf.len())
            .then(|| String::from_utf8_lossy(&buf[..n - 1]).into_owned().into())
    }
    let id = pool(|| {
        NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|s| s.to_string())
    })
    .or_else(|| {
        std::env::current_exe()
            .ok()?
            .file_name()?
            .to_str()
            .map(str::to_owned)
    });
    let Some(id) = id.filter(|n| !n.is_empty() && !n.contains('/')) else {
        return;
    };
    let saved = format!("{id}.savedState");
    let mut dirs = Vec::new();
    if let Some(c) = darwin_dir(libc::_CS_DARWIN_USER_CACHE_DIR) {
        dirs.push(c.join(&id));
    }
    if let Some(t) = darwin_dir(libc::_CS_DARWIN_USER_TEMP_DIR) {
        dirs.push(t.join(&saved));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            std::path::Path::new(&home)
                .join("Library/Saved Application State")
                .join(&saved),
        );
    }
    for d in dirs {
        if only_empty_dirs(&d) {
            remove_tree(&d);
        }
    }
    // The app stores no preferences. The shell clears what AppKit writes
    // while running (`forget_preferences`), so normally only the empty file
    // cfprefsd leaves is deleted here. If keys are still present, clear them
    // and give cfprefsd a moment to write the emptied file first; it writes
    // asynchronously, sometimes after the process has gone, which can leave
    // an empty file behind.
    let cleared_at = std::time::SystemTime::now();
    let had_keys = forget_preferences();
    if let Some(home) = std::env::var_os("HOME") {
        let plist = std::path::Path::new(&home)
            .join("Library/Preferences")
            .join(format!("{id}.plist"));
        if had_keys {
            // The write to wait for is newer than the clear and holds an
            // empty dictionary (an empty binary plist is 42 bytes).
            for _ in 0..40 {
                let emptied = std::fs::metadata(&plist)
                    .is_ok_and(|m| m.len() <= 64 && m.modified().is_ok_and(|t| t >= cleared_at));
                if emptied {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        if pref_keys().is_empty() {
            let _ = std::fs::remove_file(&plist);
        }
    }
}

/// `struct tcp_connection_info` from <netinet/tcp.h>.
#[repr(C)]
#[derive(Default)]
struct TcpConnectionInfo {
    state: u8,
    snd_wscale: u8,
    rcv_wscale: u8,
    _pad1: u8,
    options: u32,
    flags: u32,
    rto: u32,
    maxseg: u32,
    snd_ssthresh: u32,
    snd_cwnd: u32,
    snd_wnd: u32,
    snd_sbbytes: u32,
    rcv_wnd: u32,
    rttcur: u32,
    srtt: u32,
    rttvar: u32,
    tfo_bits: u32,
    txpackets: u64,
    txbytes: u64,
    txretransmitbytes: u64,
    rxpackets: u64,
    rxbytes: u64,
    rxoutoforderbytes: u64,
    txretransmitpackets: u64,
}

/// The preferences domain AppKit uses for this process.
fn pref_domain() -> Option<Retained<NSString>> {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .or_else(|| Some(NSProcessInfo::processInfo().processName()))
}

/// An autorelease pool without objc2's debug-build bookkeeping, which lives
/// in thread-local storage that is already destroyed when `atexit` handlers
/// run on the main thread.
fn pool<T>(f: impl FnOnce() -> T) -> T {
    // SAFETY: push and pop are balanced on this thread.
    let p = unsafe { objc2::ffi::objc_autoreleasePoolPush() };
    let r = f();
    // SAFETY: pops the pool pushed above.
    unsafe { objc2::ffi::objc_autoreleasePoolPop(p) };
    r
}

/// Removes the app's persistent preferences domain if it holds anything.
/// Returns whether there was something to remove.
fn forget_preferences() -> bool {
    if pref_keys().is_empty() {
        return false;
    }
    pool(|| {
        if let Some(domain) = pref_domain() {
            let defaults = NSUserDefaults::standardUserDefaults();
            defaults.removePersistentDomainForName(&domain);
            defaults.synchronize();
        }
    });
    true
}

/// Keys currently stored in the app's persistent preferences domain.
fn pref_keys() -> Vec<String> {
    pool(|| {
        let Some(domain) = pref_domain() else {
            return Vec::new();
        };
        NSUserDefaults::standardUserDefaults()
            .persistentDomainForName(&domain)
            .map(|d| d.allKeys().iter().map(|k| k.to_string()).collect())
            .unwrap_or_default()
    })
}

const RTF_GATEWAY: i32 = 0x2;
const RTA_DST: i32 = 0x1;
const RTA_GATEWAY: i32 = 0x2;
/// sizeof(struct rt_msghdr)
const RT_MSGHDR_LEN: usize = 92;

/// Default IPv4 routes as (gateway, interface index).
fn default_gateways_v4() -> Vec<(Ipv4Addr, u16)> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        libc::AF_INET,
        libc::NET_RT_FLAGS,
        RTF_GATEWAY,
    ];
    let mut len: libc::size_t = 0;
    // SAFETY: size query with a null buffer.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 || len == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; len];
    // SAFETY: buffer of the size the kernel asked for.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return Vec::new();
    }
    buf.truncate(len);
    let mut out = Vec::new();
    let mut off = 0;
    while off + RT_MSGHDR_LEN <= buf.len() {
        let msglen = usize::from(u16::from_ne_bytes([buf[off], buf[off + 1]]));
        if msglen < RT_MSGHDR_LEN || off + msglen > buf.len() {
            break;
        }
        let msg = &buf[off..off + msglen];
        off += msglen;
        let index = u16::from_ne_bytes([msg[4], msg[5]]);
        let flags = i32::from_ne_bytes([msg[8], msg[9], msg[10], msg[11]]);
        let addrs = i32::from_ne_bytes([msg[12], msg[13], msg[14], msg[15]]);
        if flags & RTF_GATEWAY == 0 {
            continue;
        }
        let mut p = RT_MSGHDR_LEN;
        let mut dst: Option<Ipv4Addr> = None;
        let mut gw: Option<Ipv4Addr> = None;
        for bit in 0..8 {
            if addrs & (1 << bit) == 0 {
                continue;
            }
            if p >= msg.len() {
                break;
            }
            let sa_len = usize::from(msg[p]);
            let fam = i32::from(msg[p + 1]);
            let v4 = (fam == libc::AF_INET && sa_len >= 8 && p + 8 <= msg.len())
                .then(|| Ipv4Addr::new(msg[p + 4], msg[p + 5], msg[p + 6], msg[p + 7]));
            if 1 << bit == RTA_DST {
                dst = v4.or((sa_len == 0).then_some(Ipv4Addr::UNSPECIFIED));
            } else if 1 << bit == RTA_GATEWAY {
                gw = v4;
            }
            p += if sa_len == 0 { 4 } else { (sa_len + 3) & !3 };
        }
        if let (Some(d), Some(g)) = (dst, gw)
            && d.is_unspecified()
        {
            out.push((g, index));
        }
    }
    out
}

fn if_index(name: &str) -> u16 {
    let Ok(c) = CString::new(name) else { return 0 };
    // SAFETY: valid C string.
    unsafe { libc::if_nametoindex(c.as_ptr()) as u16 }
}

fn mtu_of(name: &str) -> Option<u32> {
    let mut mtu = None;
    unix::for_each_ifaddr(|ifa| {
        if mtu.is_some() || ifa.ifa_addr.is_null() || ifa.ifa_data.is_null() {
            return;
        }
        // SAFETY: non-null sockaddr; for AF_LINK entries ifa_data points at
        // a struct if_data.
        unsafe {
            if i32::from((*ifa.ifa_addr).sa_family) != libc::AF_LINK {
                return;
            }
            let n = std::ffi::CStr::from_ptr(ifa.ifa_name);
            if n.to_bytes() == name.as_bytes() {
                let data = &*(ifa.ifa_data as *const libc::if_data);
                mtu = Some(data.ifi_mtu);
            }
        }
    });
    mtu
}

fn wifi_interface_names() -> Vec<String> {
    autoreleasepool(|_| {
        // SAFETY: CoreWLAN call; the shared client is safe on any thread.
        let names = unsafe { CWWiFiClient::sharedWiFiClient().interfaceNames() };
        names
            .map(|a| a.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    })
}

fn ns(s: Option<Retained<NSString>>) -> Option<String> {
    s.map(|s| s.to_string()).filter(|s| !s.is_empty())
}

fn read_wifi(iface: &CWInterface) -> WifiInfo {
    // SAFETY: CoreWLAN property getters, documented as thread-safe reads.
    unsafe {
        let channel = iface.wlanChannel();
        let rssi = iface.rssiValue();
        let noise = iface.noiseMeasurement();
        let rate = iface.transmitRate();
        WifiInfo {
            interface: ns(iface.interfaceName()),
            ssid: ns(iface.ssid()),
            bssid: ns(iface.bssid()),
            rssi_dbm: (rssi != 0).then_some(rssi as i32),
            noise_dbm: (noise != 0).then_some(noise as i32),
            channel: channel
                .as_ref()
                .map(|c| c.channelNumber() as u32)
                .filter(|&c| c != 0),
            band: channel.as_ref().and_then(|c| match c.channelBand().0 {
                1 => Some("2.4 GHz"),
                2 => Some("5 GHz"),
                3 => Some("6 GHz"),
                _ => None,
            }),
            width_mhz: channel.as_ref().and_then(|c| match c.channelWidth().0 {
                1 => Some(20),
                2 => Some(40),
                3 => Some(80),
                4 => Some(160),
                5 => Some(320),
                _ => None,
            }),
            tx_rate_mbps: (rate > 0.0).then_some(rate),
            phy_mode: match iface.activePHYMode().0 {
                1 => Some("802.11a"),
                2 => Some("802.11b"),
                3 => Some("802.11g"),
                4 => Some("802.11n"),
                5 => Some("802.11ac"),
                6 => Some("802.11ax"),
                7 => Some("802.11be"),
                _ => None,
            },
            security: match iface.security().0 {
                0 => Some("Open"),
                1 => Some("WEP"),
                2 => Some("WPA Personal"),
                3 => Some("WPA/WPA2 Personal"),
                4 => Some("WPA2 Personal"),
                5 => Some("Personal"),
                6 => Some("Dynamic WEP"),
                7 => Some("WPA Enterprise"),
                8 => Some("WPA/WPA2 Enterprise"),
                9 => Some("WPA2 Enterprise"),
                10 => Some("Enterprise"),
                11 => Some("WPA3 Personal"),
                12 => Some("WPA3 Enterprise"),
                13 => Some("WPA2/WPA3 Personal"),
                14 => Some("OWE"),
                15 => Some("OWE transition"),
                _ => None,
            },
            country: ns(iface.countryCode()),
        }
    }
}

impl Platform for MacOs {
    fn tcp_stats(&self, sock: &TcpStream) -> Option<TcpStats> {
        let mut info = TcpConnectionInfo::default();
        let mut len = std::mem::size_of::<TcpConnectionInfo>() as libc::socklen_t;
        // SAFETY: getsockopt into a correctly sized repr(C) struct.
        let r = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_CONNECTION_INFO,
                (&mut info as *mut TcpConnectionInfo).cast(),
                &mut len,
            )
        };
        if r != 0 || (len as usize) < std::mem::size_of::<TcpConnectionInfo>() {
            return None;
        }
        Some(TcpStats {
            srtt_ms: Some(f64::from(info.srtt)),
            rttvar_ms: Some(f64::from(info.rttvar)),
            rtt_cur_ms: Some(f64::from(info.rttcur)),
            mss: Some(info.maxseg),
            cwnd_bytes: Some(u64::from(info.snd_cwnd)),
            tx_packets: Some(info.txpackets),
            tx_bytes: Some(info.txbytes),
            tx_retrans_packets: Some(info.txretransmitpackets),
            tx_retrans_bytes: Some(info.txretransmitbytes),
            rx_packets: Some(info.rxpackets),
            rx_bytes: Some(info.rxbytes),
            rx_ooo_bytes: Some(info.rxoutoforderbytes),
            bytes_acked: None,
            send_queue_bytes: Some(u64::from(info.snd_sbbytes)),
        })
    }

    fn interface_for(&self, local: IpAddr) -> InterfaceInfo {
        let name = unix::interface_name_for(local);
        let wifi_names = wifi_interface_names();
        let kind = name.as_deref().and_then(|n| {
            if wifi_names.iter().any(|w| w == n) {
                Some("Wi-Fi")
            } else {
                kind_from_name(n)
            }
        });
        let gateways = default_gateways_v4();
        let idx = name.as_deref().map(if_index).unwrap_or(0);
        let gateway = gateways
            .iter()
            .find(|(_, i)| *i == idx)
            .or(gateways.first())
            .map(|(g, _)| IpAddr::V4(*g));
        InterfaceInfo {
            mtu: name.as_deref().and_then(mtu_of),
            name,
            kind,
            local_ip: Some(local),
            gateway,
        }
    }

    fn wifi(&self, interface: Option<&str>) -> Option<WifiInfo> {
        autoreleasepool(|_| {
            // SAFETY: CoreWLAN calls; the shared client is thread-safe.
            unsafe {
                let client = CWWiFiClient::sharedWiFiClient();
                let iface = match interface {
                    Some(n) => {
                        let names = client.interfaceNames()?;
                        if !names.iter().any(|s| s.to_string() == n) {
                            return None;
                        }
                        client.interfaceWithName(Some(&NSString::from_str(n)))?
                    }
                    None => client.interface()?,
                };
                if !iface.powerOn() {
                    return None;
                }
                Some(read_wifi(&iface))
            }
        })
    }

    fn ping(&self, target: IpAddr, seq: u16, timeout: Duration) -> Result<Duration, PingError> {
        unix::ping(target, seq, timeout)
    }

    fn utc_offset(&self, unix_secs: i64) -> Option<i32> {
        unix::utc_offset(unix_secs)
    }

    fn ui_font(&self) -> Option<UiFont> {
        let path = "/System/Library/Fonts/SFNS.ttf";
        let data = std::fs::read(path).ok()?;
        Some(UiFont {
            data,
            variable: true,
            semibold: None,
            has_opsz: true,
            source: path.into(),
        })
    }

    fn prepare_process(&self) {
        // Metal keeps a per-app compiled-shader cache in the user cache
        // directory; a zero size turns it off. Set before any thread exists.
        // SAFETY: called at the top of main, before other threads start.
        unsafe { std::env::set_var("MTL_SHADER_CACHE_SIZE", "0") };
        // Remove the empty per-app directories the system creates, on any
        // exit that runs atexit handlers.
        // SAFETY: registering a plain extern "C" function.
        unsafe { libc::atexit(remove_empty_state_dirs) };
        unix::exit_on_termination_signals();
        // The registration domain is volatile: these values live in memory
        // only and are never written to the app's preferences plist.
        autoreleasepool(|_| {
            // ApplePersistence = NO switches AppKit's state saving off
            // entirely (it otherwise writes global app state to
            // $TMPDIR/<bundle id>.savedState while running).
            let keys = [
                NSString::from_str("ApplePersistence"),
                NSString::from_str("NSQuitAlwaysKeepsWindows"),
                NSString::from_str("ApplePersistenceIgnoreState"),
            ];
            let no = NSNumber::new_bool(false);
            let yes = NSNumber::new_bool(true);
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let vals: [&AnyObject; 3] = [no.as_ref(), no.as_ref(), yes.as_ref()];
            let dict = NSDictionary::<NSString, AnyObject>::from_slices(&key_refs, &vals);
            // SAFETY: string keys with NSNumber values, as registerDefaults expects.
            unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&dict) };
        });
    }

    fn window_created(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        for w in app.windows().iter() {
            w.setRestorable(false);
        }
        // Refuse AppKit state restoration outright: the windowing library's
        // app delegate gets methods answering NO to "should save state".
        if let Some(delegate) = app.delegate() {
            let obj: &AnyObject = (*delegate).as_ref();
            let cls: &AnyClass = obj.class();
            let imp: Imp = {
                let f: extern "C-unwind" fn(
                    &AnyObject,
                    Sel,
                    *mut AnyObject,
                    *mut AnyObject,
                ) -> Bool = refuse;
                // SAFETY: an Objective-C method IMP with the matching signature.
                unsafe { std::mem::transmute::<_, Imp>(f) }
            };
            #[cfg(target_arch = "aarch64")]
            let types = c"B@:@@";
            #[cfg(not(target_arch = "aarch64"))]
            let types = c"c@:@@";
            for name in [
                sel!(application:shouldSaveApplicationState:),
                sel!(application:shouldSaveSecureApplicationState:),
            ] {
                // SAFETY: adding a method to a live class; fails harmlessly
                // if the class already implements it.
                unsafe {
                    objc2::ffi::class_addMethod(
                        cls as *const AnyClass as *mut AnyClass,
                        name,
                        imp,
                        types.as_ptr(),
                    );
                }
            }
            // Re-set so AppKit re-reads which delegate methods exist.
            app.setDelegate(Some(&delegate));
        }
    }

    fn tray_supported(&self) -> bool {
        true
    }

    fn tray_set(&self, on: bool, wake: super::Wake) {
        macos_tray::set(on, wake);
    }

    fn tray_update(&self, status: &super::TrayStatus) {
        macos_tray::update(status);
    }

    fn tray_take_actions(&self) -> Vec<super::TrayAction> {
        macos_tray::take_actions()
    }

    fn set_dock_icon(&self, visible: bool) {
        macos_tray::set_dock_icon(visible);
    }

    fn activate(&self) {
        macos_tray::activate();
    }

    fn quit(&self) {
        macos_tray::quit();
    }

    fn forget_preferences(&self) {
        forget_preferences();
    }

    fn choose_save_path(&self, suggested_name: &str) -> Result<Option<PathBuf>, String> {
        let mtm = MainThreadMarker::new().ok_or("save panel needs the main thread")?;
        // The save panel records its last folder in the app's preferences
        // (NSOSPLastRootDirectory). The app keeps no preferences of its own,
        // so whatever keys the panel adds are removed again afterwards.
        let before = pref_keys();
        let result = autoreleasepool(|_| {
            let panel = NSSavePanel::savePanel(mtm);
            panel.setNameFieldStringValue(&NSString::from_str(suggested_name));
            panel.setCanCreateDirectories(true);
            panel.setShowsTagField(false);
            if panel.runModal() != NSModalResponseOK {
                return Ok(None);
            }
            let path = panel
                .URL()
                .and_then(|u| u.path())
                .map(|p| PathBuf::from(p.to_string()));
            Ok(path)
        });
        autoreleasepool(|_| {
            let defaults = NSUserDefaults::standardUserDefaults();
            let Some(domain) = pref_domain() else { return };
            if before.is_empty() {
                defaults.removePersistentDomainForName(&domain);
            } else {
                for k in pref_keys() {
                    if !before.contains(&k) {
                        defaults.removeObjectForKey(&NSString::from_str(&k));
                    }
                }
            }
            defaults.synchronize();
        });
        result
    }
}
