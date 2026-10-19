//! Connection and server info column. Hovering a value shows its source.

use std::net::IpAddr;

use eframe::egui::{Align2, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::charts::{Decimator, MeshCache, key_of, segments};

use super::text::{self, Text};
use super::widgets::{Tip, tooltip};
use super::{format, theme};
use crate::app::derived::Derived;
use crate::app::session::Session;
use crate::model::{Load, Sourced};
use crate::net::cloudflare;

struct Rows<'a> {
    text: &'a Text,
}

impl Rows<'_> {
    fn section(&self, ui: &mut Ui, label: &str) {
        ui.add_space(theme::S8);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), theme::S24), Sense::hover());
        self.text.paint(
            ui.painter(),
            pos2(r.left(), r.center().y),
            Align2::LEFT_CENTER,
            label,
            text::SECTION,
        );
    }

    /// A row whose hover explains the value's source.
    fn row(&self, ui: &mut Ui, label: &str, value: Option<&str>, source: &str) {
        let resp = super::widgets::kv_row(ui, self.text, label, value, text::BODY);
        if let Some(hp) = resp.hover_pos()
            && !source.is_empty()
        {
            tooltip(
                ui,
                self.text,
                hp,
                &Tip::default()
                    .title(label.to_owned())
                    .note(source.to_owned()),
            );
        }
    }

    fn sourced(&self, ui: &mut Ui, label: &str, v: &Option<Sourced>, missing: &str) {
        match v {
            Some(s) => self.row(ui, label, Some(&s.value), &format!("Source: {}", s.source)),
            None => self.row(ui, label, None, missing),
        }
    }
}

fn family(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(_) => "IPv4",
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some() => "IPv4",
        IpAddr::V6(_) => "IPv6",
    }
}

pub fn show(ui: &mut Ui, text: &Text, s: &Session, d: &Derived, st: &mut State) {
    let rows = Rows { text };
    let e = &s.edge;
    let last_conn = s.conns.last();
    let pending = "Measured when a test runs.";

    rows.section(ui, "Server");
    let colo = e.colo.as_ref().map(|c| match &e.colo_city {
        Some(city) => format!("{} · {}", c.value, city.value),
        None => c.value.clone(),
    });
    rows.row(
        ui,
        "Data center",
        colo.as_deref(),
        &e.colo.as_ref().map_or(pending.to_owned(), |c| {
            format!("IATA code from {}", c.source)
        }),
    );
    rows.row(
        ui,
        "Host",
        Some(cloudflare::HOST),
        "All HTTP tests run against this host.",
    );
    let remote = last_conn.map(|c| c.remote.ip().to_string());
    rows.row(
        ui,
        "Address",
        remote.as_deref(),
        "Peer address of the most recent TCP connection.",
    );
    rows.sourced(ui, "HTTP", &e.http, pending);
    let tls = last_conn
        .map(|c| c.tls_version.clone())
        .filter(|v| !v.is_empty());
    rows.row(
        ui,
        "TLS",
        tls.as_deref(),
        "Negotiated by this app's TLS client (rustls).",
    );
    let cipher = last_conn
        .map(|c| c.cipher.clone())
        .filter(|v| !v.is_empty());
    rows.row(
        ui,
        "Cipher",
        cipher.as_deref(),
        "Negotiated by this app's TLS client (rustls).",
    );
    rows.sourced(ui, "Key exchange", &e.kex, pending);

    rows.section(ui, "Client");
    rows.sourced(ui, "Public IP", &e.client_ip, pending);
    rows.sourced(ui, "UDP mapping", &e.udp_mapped, pending);
    let network = e.asn.as_ref().map(|a| match &e.as_org {
        Some(o) => format!("{} · {}", a.value, o.value),
        None => a.value.clone(),
    });
    rows.row(
        ui,
        "Network",
        network.as_deref(),
        &e.asn.as_ref().map_or(pending.to_owned(), |a| {
            format!(
                "Source: {}. The network name comes only from /meta, which answers with no data to clients that don't send the web test's Referer.",
                a.source
            )
        }),
    );
    let loc = match (&e.client_city, &e.client_region, &e.client_country) {
        (None, None, None) => None,
        (c, r, k) => Some(
            [c, r, k]
                .iter()
                .filter_map(|x| x.as_ref().map(|x| x.value.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    rows.row(
        ui,
        "Location",
        loc.as_deref(),
        &e.client_city
            .as_ref()
            .or(e.client_country.as_ref())
            .map_or(pending.to_owned(), |c| {
                format!(
                    "Cloudflare's geolocation of the public IP. Source: {}",
                    c.source
                )
            }),
    );
    rows.sourced(ui, "Coordinates", &e.client_coords, pending);
    rows.sourced(ui, "Time zone", &e.timezone, pending);
    rows.sourced(ui, "WARP", &e.warp, pending);

    rows.section(ui, "Connection");
    let local = last_conn.map(|c| c.local.ip());
    rows.row(
        ui,
        "IP version",
        local.map(family),
        "Address family of the most recent TCP connection.",
    );
    let host = s.host.as_ref();
    for (label, res, addrs) in [
        (
            "IPv4 connect",
            host.and_then(|h| h.v4_connect_ms.clone()),
            host.map(|h| &h.v4),
        ),
        (
            "IPv6 connect",
            host.and_then(|h| h.v6_connect_ms.clone()),
            host.map(|h| &h.v6),
        ),
    ] {
        let list = addrs
            .map(|a| {
                a.iter()
                    .map(IpAddr::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        match res {
            Some(Ok(ms)) => rows.row(
                ui,
                label,
                Some(&format!("{} ms", format::ms(ms))),
                &format!("TCP connect time to the first resolved address ({list})."),
            ),
            Some(Err(err)) => rows.row(ui, label, Some(super::widgets::unavailable()), &err),
            None => rows.row(ui, label, None, pending),
        }
    }
    let dns = host
        .and_then(|h| h.dns_ms)
        .map(|v| format!("{} ms", format::ms(v)));
    rows.row(
        ui,
        "DNS lookup",
        dns.as_deref(),
        "System resolver (getaddrinfo), timed by this app.",
    );
    let iface = s.iface.as_ref();
    let local_s = iface.and_then(|i| i.local_ip).map(|i| i.to_string());
    rows.row(
        ui,
        "Local IP",
        local_s.as_deref(),
        "Local address of the most recent TCP connection.",
    );
    rows.row(
        ui,
        "Interface",
        iface.and_then(|i| i.name.as_deref()),
        "Interface holding the local address (getifaddrs).",
    );
    rows.row(
        ui,
        "Type",
        iface.and_then(|i| i.kind),
        "Wi-Fi from the system's wireless interface list, otherwise from the interface name.",
    );
    let mtu = iface.and_then(|i| i.mtu).map(|m| m.to_string());
    rows.row(
        ui,
        "MTU",
        mtu.as_deref(),
        "Interface MTU in bytes from the link-layer address entry.",
    );
    let gw = iface.and_then(|i| i.gateway).map(|g| g.to_string());
    rows.row(
        ui,
        "Gateway",
        gw.as_deref(),
        "Default IPv4 route of the interface (routing table).",
    );
    let gwl = d.gw_by_load[Load::Idle.as_u8() as usize]
        .summary
        .map(|s| format!("{} ms", format::ms(s.median)));
    rows.row(
        ui,
        "Gateway latency",
        gwl.as_deref(),
        "Median ICMP echo round trip to the gateway while the link is unloaded. macOS answers \"No route to host\" to apps without Local Network access (Privacy & Security settings).",
    );

    match &s.wifi {
        Some(Some(w)) => {
            rows.section(ui, "Wi-Fi");
            let loc_note = "macOS returns this only to apps with Location Services access; this app does not request it.";
            rows.row(
                ui,
                "Network",
                w.ssid.as_deref().or(Some(super::widgets::unavailable())),
                if w.ssid.is_some() {
                    "CoreWLAN"
                } else {
                    loc_note
                },
            );
            rows.row(
                ui,
                "BSSID",
                w.bssid.as_deref().or(Some(super::widgets::unavailable())),
                if w.bssid.is_some() {
                    "CoreWLAN"
                } else {
                    loc_note
                },
            );
            let rssi = w.rssi_dbm.map(|v| format!("{v} dBm"));
            rows.row(
                ui,
                "RSSI",
                rssi.as_deref(),
                "Received signal strength reported by CoreWLAN, sampled every 2 s.",
            );
            let noise = w.noise_dbm.map(|v| format!("{v} dBm"));
            rows.row(
                ui,
                "Noise",
                noise.as_deref(),
                "Noise floor reported by CoreWLAN.",
            );
            let snr = match (w.rssi_dbm, w.noise_dbm) {
                (Some(r), Some(n)) => Some(format!("{} dB", r - n)),
                _ => None,
            };
            rows.row(ui, "SNR", snr.as_deref(), "RSSI minus noise.");
            let ch = w.channel.map(|c| {
                let mut v = c.to_string();
                if let Some(b) = w.band {
                    v.push_str(&format!(" · {b}"));
                }
                if let Some(wd) = w.width_mhz {
                    v.push_str(&format!(" · {wd} MHz"));
                }
                v
            });
            rows.row(
                ui,
                "Channel",
                ch.as_deref(),
                "CoreWLAN channel number, band and width.",
            );
            rows.row(
                ui,
                "PHY mode",
                w.phy_mode,
                "Active PHY mode reported by CoreWLAN.",
            );
            let rate = w.tx_rate_mbps.map(|r| format!("{} Mbps", format::sig3(r)));
            rows.row(
                ui,
                "Tx rate",
                rate.as_deref(),
                "Current transmit rate reported by CoreWLAN.",
            );
            rows.row(ui, "Security", w.security, "CoreWLAN security mode.");
            rssi_chart(ui, text, s, st);
        }
        Some(None) if iface.and_then(|i| i.kind) == Some("Wi-Fi") => {
            rows.section(ui, "Wi-Fi");
            rows.row(
                ui,
                "Wi-Fi",
                Some(super::widgets::unavailable()),
                "This platform does not report Wi-Fi details yet.",
            );
        }
        _ => {}
    }
}

/// Cached geometry for the RSSI sparkline.
#[derive(Default)]
pub struct State {
    rssi: MeshCache,
    pts: Vec<Pos2>,
    breaks: Vec<usize>,
}

/// RSSI over the run, with hover values.
fn rssi_chart(ui: &mut Ui, text: &Text, s: &Session, st: &mut State) {
    let raw = &s.wifi_samples.raw;
    if raw.len() < 2 {
        return;
    }
    ui.add_space(theme::S8);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::S32 + theme::S16),
        Sense::hover(),
    );
    let p = ui.painter_at(rect.expand(2.0));
    let (Some(lo), Some(hi)) = (s.wifi_samples.hist.min(), s.wifi_samples.hist.max()) else {
        return;
    };
    let (lo, hi) = ((lo - 2.0).floor(), (hi + 2.0).ceil());
    let t0 = raw.get(0).map_or(0.0, |w| w.t);
    let t1 = raw.last().map_or(1.0, |w| w.t).max(t0 + 1.0);
    let plot = Rect::from_min_max(
        pos2(rect.left() + theme::S32, rect.top() + theme::S8),
        rect.max,
    );
    let x = |t: f64| plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
    let y = |v: f64| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();
    p.hline(
        plot.x_range(),
        plot.bottom() + 0.5,
        Stroke::new(theme::STROKE, theme::BORDER),
    );
    text.paint(
        &p,
        pos2(plot.left() - theme::S4, y(hi)),
        Align2::RIGHT_TOP,
        &format!("{hi:.0}"),
        text::SMALL_2,
    );
    text.paint(
        &p,
        pos2(plot.left() - theme::S4, y(lo)),
        Align2::RIGHT_BOTTOM,
        &format!("{lo:.0}"),
        text::SMALL_2,
    );
    let key = key_of((
        s.version,
        plot.min.x.to_bits(),
        plot.min.y.to_bits(),
        plot.max.x.to_bits(),
        plot.max.y.to_bits(),
    ));
    let (pts, breaks) = (&mut st.pts, &mut st.breaks);
    st.rssi.paint(ui, &p, key, |b| {
        let mut dec = Decimator::new(pts, breaks);
        for w in raw.iter() {
            match w.rssi_dbm {
                Some(r) => dec.push(pos2(x(w.t), y(f64::from(r)))),
                None => dec.gap(),
            }
        }
        dec.finish();
        for (i, j) in segments(breaks) {
            b.polyline(&pts[i..j], theme::DATA_LINE, theme::TEXT);
        }
    });
    text.paint(
        &p,
        pos2(plot.right(), rect.top()),
        Align2::RIGHT_TOP,
        "RSSI dBm",
        text::SMALL_2,
    );
    let _ = ui.interact(plot, Id::unique("rssi"), Sense::hover());
    if let Some(hp) = resp.hover_pos().filter(|h| plot.contains(*h)) {
        let t = t0 + f64::from((hp.x - plot.left()) / plot.width()) * (t1 - t0);
        let i = raw.partition_point(|w| w.t < t).min(raw.len() - 1);
        if let Some(w) = raw.get(i)
            && let Some(r) = w.rssi_dbm
        {
            let pt = pos2(x(w.t), y(f64::from(r)));
            p.circle(
                pt,
                theme::POINT_HOVER_RADIUS,
                theme::TEXT,
                Stroke::new(theme::STROKE, theme::BG),
            );
            let mut tip = Tip::default()
                .title("Wi-Fi sample")
                .pair("RSSI", format!("{r} dBm"));
            if let Some(n) = w.noise_dbm {
                tip.push_pair("Noise", format!("{n} dBm"));
            }
            if let Some(tx) = w.tx_rate_mbps {
                tip.push_pair("Tx rate", format!("{} Mbps", format::sig3(tx)));
            }
            if let Some(wall) = s.wall_at(w.t) {
                tip.push_pair("Time", format::clock(wall, true));
            }
            tooltip(ui, text, pt, &tip);
        }
    }
}
