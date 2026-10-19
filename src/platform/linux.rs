use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use super::unix::{self, kind_from_name};
use super::{PingError, Platform, UiFont};
use crate::model::{InterfaceInfo, TcpStats, WifiInfo};

pub struct Linux;

/// Kernel `struct tcp_info` up to `tcpi_bytes_retrans` (Linux 4.19+).
/// Older kernels return a shorter struct; fields past the returned length
/// stay zero and are reported as unavailable.
#[repr(C)]
#[derive(Default)]
struct TcpInfo {
    state: u8,
    ca_state: u8,
    retransmits: u8,
    probes: u8,
    backoff: u8,
    options: u8,
    wscale: u8,
    flags: u8,
    rto: u32,
    ato: u32,
    snd_mss: u32,
    rcv_mss: u32,
    unacked: u32,
    sacked: u32,
    lost: u32,
    retrans: u32,
    fackets: u32,
    last_data_sent: u32,
    last_ack_sent: u32,
    last_data_recv: u32,
    last_ack_recv: u32,
    pmtu: u32,
    rcv_ssthresh: u32,
    rtt: u32,
    rttvar: u32,
    snd_ssthresh: u32,
    snd_cwnd: u32,
    advmss: u32,
    reordering: u32,
    rcv_rtt: u32,
    rcv_space: u32,
    total_retrans: u32,
    pacing_rate: u64,
    max_pacing_rate: u64,
    bytes_acked: u64,
    bytes_received: u64,
    segs_out: u32,
    segs_in: u32,
    notsent_bytes: u32,
    min_rtt: u32,
    data_segs_in: u32,
    data_segs_out: u32,
    delivery_rate: u64,
    busy_time: u64,
    rwnd_limited: u64,
    sndbuf_limited: u64,
    delivered: u32,
    delivered_ce: u32,
    bytes_sent: u64,
    bytes_retrans: u64,
}

fn default_gateway() -> Option<(String, Ipv4Addr)> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    routes.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 || f[1] != "00000000" {
            return None;
        }
        let gw = u32::from_str_radix(f[2], 16).ok()?;
        Some((f[0].to_owned(), Ipv4Addr::from(u32::from_be(gw))))
    })
}

impl Platform for Linux {
    fn tcp_stats(&self, sock: &TcpStream) -> Option<TcpStats> {
        let mut info = TcpInfo::default();
        let mut len = std::mem::size_of::<TcpInfo>() as libc::socklen_t;
        // SAFETY: getsockopt into a repr(C) struct of the given size.
        let r = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_INFO,
                (&mut info as *mut TcpInfo).cast(),
                &mut len,
            )
        };
        if r != 0 {
            return None;
        }
        let have = |end: usize| (len as usize) >= end;
        Some(TcpStats {
            srtt_ms: Some(f64::from(info.rtt) / 1000.0),
            rttvar_ms: Some(f64::from(info.rttvar) / 1000.0),
            rtt_cur_ms: None,
            mss: Some(info.snd_mss),
            cwnd_bytes: Some(u64::from(info.snd_cwnd) * u64::from(info.snd_mss)),
            tx_packets: have(140).then_some(u64::from(info.segs_out)),
            tx_bytes: have(208).then_some(info.bytes_sent),
            tx_retrans_packets: Some(u64::from(info.total_retrans)),
            tx_retrans_bytes: have(216).then_some(info.bytes_retrans),
            rx_packets: have(144).then_some(u64::from(info.segs_in)),
            rx_bytes: have(136).then_some(info.bytes_received),
            rx_ooo_bytes: None,
            bytes_acked: have(128).then_some(info.bytes_acked),
            send_queue_bytes: None,
        })
    }

    fn interface_for(&self, local: IpAddr) -> InterfaceInfo {
        let name = unix::interface_name_for(local);
        let mtu = name.as_deref().and_then(|n| {
            std::fs::read_to_string(format!("/sys/class/net/{n}/mtu"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
        });
        let wireless = name.as_deref().is_some_and(|n| {
            std::path::Path::new(&format!("/sys/class/net/{n}/wireless")).exists()
        });
        let gateway = default_gateway()
            .filter(|(iface, _)| name.as_deref().is_none_or(|n| n == iface))
            .map(|(_, g)| IpAddr::V4(g));
        InterfaceInfo {
            kind: if wireless {
                Some("Wi-Fi")
            } else {
                name.as_deref().and_then(kind_from_name)
            },
            name,
            mtu,
            local_ip: Some(local),
            gateway,
        }
    }

    fn wifi(&self, _interface: Option<&str>) -> Option<WifiInfo> {
        // nl80211 access is not implemented yet.
        None
    }

    fn ping(&self, target: IpAddr, seq: u16, timeout: Duration) -> Result<Duration, PingError> {
        unix::ping(target, seq, timeout)
    }

    fn utc_offset(&self, unix_secs: i64) -> Option<i32> {
        unix::utc_offset(unix_secs)
    }

    fn ui_font(&self) -> Option<UiFont> {
        const CANDIDATES: [(&str, Option<&str>); 6] = [
            ("/usr/share/fonts/cantarell/Cantarell-VF.otf", None),
            (
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                Some("/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf"),
            ),
            (
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                Some("/usr/share/fonts/noto/NotoSans-SemiBold.ttf"),
            ),
            (
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                Some("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
            ),
            (
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                Some("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"),
            ),
            (
                "/usr/share/fonts/dejavu/DejaVuSans.ttf",
                Some("/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf"),
            ),
        ];
        CANDIDATES.iter().find_map(|(path, bold)| {
            let data = std::fs::read(path).ok()?;
            let variable = path.contains("-VF");
            Some(UiFont {
                data,
                variable,
                semibold: if variable {
                    None
                } else {
                    bold.and_then(|b| std::fs::read(b).ok())
                },
                has_opsz: false,
                source: (*path).into(),
            })
        })
    }

    fn prepare_process(&self) {
        unix::exit_on_termination_signals();
    }

    fn choose_save_path(&self, suggested_name: &str) -> Result<Option<PathBuf>, String> {
        // The desktop's own save dialog, through whichever helper exists.
        let tries: [(&str, Vec<String>); 2] = [
            (
                "zenity",
                vec![
                    "--file-selection".into(),
                    "--save".into(),
                    "--confirm-overwrite".into(),
                    format!("--filename={suggested_name}"),
                ],
            ),
            (
                "kdialog",
                vec!["--getsavefilename".into(), suggested_name.into()],
            ),
        ];
        for (cmd, args) in tries {
            match std::process::Command::new(cmd).args(&args).output() {
                Ok(out) if out.status.success() => {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                    return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
                }
                // Cancelled.
                Ok(_) => return Ok(None),
                Err(_) => continue,
            }
        }
        Err("no save dialog available (install zenity or kdialog)".into())
    }
}
