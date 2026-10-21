//! Everything outside the charts, as plain values: headline numbers,
//! status, control state, settings, breakdowns, connection info and the
//! menu bar summary. Built from one session snapshot.

use std::net::IpAddr;
use std::time::Duration;

use super::chart::boxplot::{self, BoxId};
use super::format;
use super::palette::{Color, GATEWAY};
use super::tip::Tip;
use crate::app::derived::{Derived, Headline as Stat, Retrans};
use crate::app::session::{RunState, Session, dir_idx};
use crate::model::{Config, Direction, IpFamily, Load, Phase, RunMode, Sourced};
use crate::net::cloudflare;

/// Before a value is measured.
pub const MISSING: &str = "—";
/// When a platform can't measure a value.
pub const UNAVAILABLE: &str = "Unavailable";

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub headline: Vec<Headline>,
    pub status: Status,
    /// Progress line fraction; `None` shows an empty track.
    pub progress: Option<f32>,
    /// Run time, once a run has started.
    pub elapsed: Option<String>,
    pub controls: Controls,
    pub settings: Settings,
    /// Download, upload, latency, packet loss.
    pub columns: Vec<Column>,
    pub info: Vec<Section>,
    /// Whether to show the RSSI sparkline after the Wi-Fi section.
    pub rssi: bool,
    pub log: LogCounts,
    pub tray: Tray,
    pub legends: Legends,
    /// File name suggested for an export.
    pub export_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Headline {
    pub label: &'static str,
    /// `None` before it is measured (shown as `MISSING`, with no unit).
    pub value: Option<String>,
    pub unit: &'static str,
    /// Series colour, for the swatch next to the label.
    pub color: Color,
    /// Download and upload are hero values; the rest secondary.
    pub hero: bool,
    pub tip: Tip,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub text: String,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
    pub mode: RunMode,
    /// The mode can be changed only while no run is active.
    pub mode_enabled: bool,
    /// Running or paused: Stop and Pause/Resume apply.
    pub active: bool,
    pub paused: bool,
    pub can_reset: bool,
    pub can_export: bool,
}

/// A labelled value with the tooltip explaining it.
#[derive(Clone, Debug, PartialEq)]
pub struct Kv {
    pub label: String,
    /// `None` before it is measured.
    pub value: Option<String>,
    pub tip: Tip,
}

fn kv(label: &str, value: Option<String>, how: &str) -> Kv {
    Kv {
        label: label.to_owned(),
        value,
        tip: Tip::default().title(label.to_owned()).note(how.to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A small label above the items that follow.
    Sub(&'static str),
    /// A box plot with this many rows.
    Boxes {
        id: BoxId,
        rows: usize,
    },
    Kv(Kv),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub title: &'static str,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Kv>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogCounts {
    pub requests: usize,
    pub connections: usize,
    pub errors: usize,
}

/// The menu bar (or tray) summary.
#[derive(Clone, Debug, PartialEq)]
pub struct Tray {
    /// Compact value next to the icon (latency); empty shows the icon only.
    pub title: String,
    /// `(label, value)` rows: download, upload, latency, jitter, loss.
    pub rows: Vec<(String, String)>,
    pub status: String,
    pub one_shot_running: bool,
    pub continuous_running: bool,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Legend {
    pub label: &'static str,
    pub color: Color,
    /// Drawn as a short vertical tick (loss) instead of a line sample.
    pub tick: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Legends {
    pub throughput: Vec<Legend>,
    pub latency: Vec<Legend>,
}

// ---- Settings ----

/// A continuous-mode setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Setting {
    Interval = 0,
    Sustain = 1,
    Streams = 2,
    LatencyInterval = 3,
    UdpProbes = 4,
    IpVersion = 5,
}

impl Setting {
    pub const ALL: [Setting; 6] = [
        Setting::Interval,
        Setting::Sustain,
        Setting::Streams,
        Setting::LatencyInterval,
        Setting::UdpProbes,
        Setting::IpVersion,
    ];

    // Not every shell uses every part of the view layer.
    #[allow(dead_code)]
    pub fn from_u8(v: u8) -> Option<Setting> {
        Setting::ALL.get(usize::from(v)).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            Setting::Interval => "Throughput every",
            Setting::Sustain => "Transfer for",
            Setting::Streams => "Connections",
            Setting::LatencyInterval => "Latency probe every",
            Setting::UdpProbes => "UDP probes",
            Setting::IpVersion => "IP version",
        }
    }

    pub fn options(self) -> Vec<String> {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect();
        match self {
            Setting::Interval => s(&INTERVALS.map(|x| x.1)),
            Setting::Sustain => s(&SUSTAIN.map(|x| x.1)),
            Setting::Streams => STREAMS.iter().map(u32::to_string).collect(),
            Setting::LatencyInterval => s(&LATENCY_MS.map(|x| x.1)),
            Setting::UdpProbes => s(&LOSS_HZ.map(|x| x.1)),
            Setting::IpVersion => s(&FAMILIES.map(|f| f.label())),
        }
    }

    /// The option `cfg` currently has, or the default's position.
    pub fn selected(self, cfg: &Config) -> usize {
        let c = &cfg.continuous;
        match self {
            Setting::Interval => INTERVALS
                .iter()
                .position(|(s, _)| Duration::from_secs(*s) == c.cycle_interval)
                .unwrap_or(4),
            Setting::Sustain => SUSTAIN
                .iter()
                .position(|(secs, _)| Duration::from_secs(*secs) == c.sustain)
                .unwrap_or(1),
            Setting::Streams => STREAMS.iter().position(|n| *n == cfg.streams).unwrap_or(2),
            Setting::LatencyInterval => LATENCY_MS
                .iter()
                .position(|(ms, _)| Duration::from_millis(*ms) == c.latency_interval)
                .unwrap_or(1),
            Setting::UdpProbes => LOSS_HZ
                .iter()
                .position(|(hz, _)| (*hz - c.loss_rate_hz).abs() < 1e-9)
                .unwrap_or(3),
            Setting::IpVersion => FAMILIES.iter().position(|f| *f == cfg.family).unwrap_or(0),
        }
    }

    /// `cfg` with option `i` chosen; `None` if `i` is out of range.
    pub fn apply(self, cfg: &Config, i: usize) -> Option<Config> {
        let mut out = *cfg;
        let c = &mut out.continuous;
        match self {
            Setting::Interval => c.cycle_interval = Duration::from_secs(INTERVALS.get(i)?.0),
            Setting::Sustain => c.sustain = Duration::from_secs(SUSTAIN.get(i)?.0),
            Setting::Streams => out.streams = *STREAMS.get(i)?,
            Setting::LatencyInterval => {
                c.latency_interval = Duration::from_millis(LATENCY_MS.get(i)?.0)
            }
            Setting::UdpProbes => c.loss_rate_hz = LOSS_HZ.get(i)?.0,
            Setting::IpVersion => out.family = *FAMILIES.get(i)?,
        }
        Some(out)
    }
}

const INTERVALS: [(u64, &str); 7] = [
    (60, "1 min"),
    (120, "2 min"),
    (300, "5 min"),
    (600, "10 min"),
    (900, "15 min"),
    (1800, "30 min"),
    (3600, "60 min"),
];
const SUSTAIN: [(u64, &str); 4] = [(2, "2 s"), (5, "5 s"), (10, "10 s"), (15, "15 s")];
const STREAMS: [u32; 4] = [1, 2, 4, 8];
const LATENCY_MS: [(u64, &str); 5] = [
    (500, "0.5 s"),
    (1000, "1 s"),
    (2000, "2 s"),
    (5000, "5 s"),
    (10_000, "10 s"),
];
const LOSS_HZ: [(f64, &str); 6] = [
    (0.0, "Off"),
    (1.0, "1/s"),
    (2.0, "2/s"),
    (5.0, "5/s"),
    (10.0, "10/s"),
    (20.0, "20/s"),
];
const FAMILIES: [IpFamily; 3] = [IpFamily::Auto, IpFamily::V4, IpFamily::V6];

#[derive(Clone, Debug, PartialEq)]
pub struct OptionRow {
    pub id: Setting,
    pub label: &'static str,
    pub options: Vec<String>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub rows: Vec<OptionRow>,
    /// Throughput data per hour at these settings.
    pub data_per_hour: Kv,
}

fn settings(cfg: &Config, d: &Derived) -> Settings {
    let rows = Setting::ALL
        .into_iter()
        .map(|id| OptionRow {
            id,
            label: id.label(),
            options: id.options(),
            selected: id.selected(cfg),
        })
        .collect();
    let c = &cfg.continuous;
    let per_cycle = match (d.download.value, d.upload.value) {
        (Some(a), Some(b)) => Some((a + b) / 8.0 * c.sustain.as_secs_f64()),
        _ => None,
    };
    let per_hour = per_cycle.map(|b| b * 3600.0 / c.cycle_interval.as_secs_f64());
    let value = per_hour.map(|b| {
        if b >= 1e9 {
            format!("≈ {} GB", format::sig3(b / 1e9))
        } else {
            format!("≈ {} MB", format::sig3(b / 1e6))
        }
    });
    Settings {
        rows,
        data_per_hour: Kv {
            label: "Throughput data per hour".into(),
            value,
            tip: Tip::default()
                .note("Last measured download and upload rates × transfer time × cycles per hour."),
        },
    }
}

// ---- Status ----

/// Status line text and colour. A notice (export result) wins; then an
/// error from the last 10 s of an active run; then the run state.
pub fn status(s: &Session, notice: Option<(&str, bool)>) -> Status {
    let st = |text: String, color: Color| Status { text, color };
    let now = s.now().unwrap_or(0.0);
    if let Some((msg, err)) = notice {
        return st(msg.to_owned(), if err { Color::Error } else { Color::Text });
    }
    if s.active()
        && let Some((t, msg)) = s.errors.last()
        && now - t < 10.0
    {
        return st(msg.clone(), Color::Error);
    }
    match s.state {
        RunState::Idle => st("Idle".into(), Color::Text2),
        RunState::Paused => st("Paused".into(), Color::Text),
        RunState::Finished => st(
            format!(
                "Finished · {}",
                format::elapsed(s.finished_at.unwrap_or(0.0))
            ),
            Color::Text,
        ),
        RunState::Running => match &s.phase {
            None => st("Starting".into(), Color::Text),
            Some(Phase::Connecting) => {
                st(format!("Connecting · {}", cloudflare::HOST), Color::Text)
            }
            Some(Phase::Latency { done, total }) => st(
                format!("Measuring latency · {done}/{total} probes"),
                Color::Text,
            ),
            Some(Phase::Sustained {
                dir,
                streams,
                started,
                length,
            }) => st(
                format!(
                    "{} · {streams} connections · {:.0} of {:.0} s",
                    dir.label(),
                    (now - started).clamp(0.0, *length).floor(),
                    length
                ),
                Color::Text,
            ),
            Some(Phase::PacketLoss { sent, total }) => st(
                format!("Measuring packet loss · {sent}/{total} probes"),
                Color::Text,
            ),
            Some(Phase::Waiting { next_cycle_at }) => st(
                format!(
                    "Next throughput cycle in {}",
                    format::elapsed((next_cycle_at - now).ceil())
                ),
                Color::Text,
            ),
            Some(Phase::BackingOff { until, reason }) => st(
                format!(
                    "Retrying in {} · {reason}",
                    format::elapsed((until - now).ceil())
                ),
                Color::Warning,
            ),
        },
    }
}

fn progress(s: &Session) -> Option<f32> {
    match (s.mode, s.state) {
        (Some(RunMode::OneShot), RunState::Running | RunState::Paused) => Some(s.progress),
        (Some(RunMode::OneShot), RunState::Finished) => Some(1.0),
        (Some(RunMode::Continuous), RunState::Running | RunState::Paused) => match &s.phase {
            Some(Phase::Sustained {
                started, length, ..
            }) => {
                let now = s.now().unwrap_or(0.0);
                Some(((now - started) / length.max(1e-3)).clamp(0.0, 1.0) as f32)
            }
            Some(Phase::Waiting { next_cycle_at }) => {
                let iv = s.config.continuous.cycle_interval.as_secs_f64().max(1.0);
                let now = s.now().unwrap_or(0.0);
                Some((1.0 - ((next_cycle_at - now) / iv).clamp(0.0, 1.0)) as f32)
            }
            _ => None,
        },
        _ => None,
    }
}

// ---- Headline ----

fn stats_tip(h: &Stat, unit: &str, fmt: fn(f64) -> String, how: &str) -> Tip {
    let mut t = Tip::default();
    if let Some(s) = h.summary {
        let v = |x: f64| format!("{} {unit}", fmt(x));
        t.push_pair("Min", v(s.min));
        t.push_pair("Median", v(s.median));
        t.push_pair("p90", v(s.p90));
        t.push_pair("p99", v(s.p99));
        t.push_pair("Max", v(s.max));
        t.push_pair("Samples", format::count(s.n as u64));
    }
    t.note(format!("{}. {how}", h.basis))
}

fn headline(d: &Derived) -> Vec<Headline> {
    let lc = d.loss_basis;
    let mut loss_tip = Tip::default()
        .pair("Sent", format::count(lc.sent()))
        .pair("Lost", format::count(lc.lost));
    if lc.late > 0 {
        loss_tip.push_pair("Late answers", format::count(lc.late));
    }
    if lc.duplicate > 0 {
        loss_tip.push_pair("Duplicates", format::count(lc.duplicate));
    }
    let loss_tip = loss_tip.note(format!(
        "{}. STUN Binding requests to stun.cloudflare.com:3478; each is answered once, so a missing answer is a packet lost in either direction.",
        d.loss.basis
    ));
    vec![
        Headline {
            label: "Download",
            value: d.download.value.map(format::mbps),
            unit: "Mbps",
            color: Color::Download,
            hero: true,
            tip: stats_tip(
                &d.download,
                "Mbps",
                format::mbps,
                "Per request: body bits ÷ (TTFB − server time + transfer time), as Cloudflare computes it.",
            ),
        },
        Headline {
            label: "Upload",
            value: d.upload.value.map(format::mbps),
            unit: "Mbps",
            color: Color::Upload,
            hero: true,
            tip: stats_tip(
                &d.upload,
                "Mbps",
                format::mbps,
                "Per request: body bits ÷ time from request start to the first response byte.",
            ),
        },
        Headline {
            label: "Latency",
            value: d.latency.value.map(format::ms),
            unit: "ms",
            color: Color::Latency,
            hero: false,
            tip: stats_tip(
                &d.latency,
                "ms",
                format::ms,
                "Each probe: TTFB of GET __down?bytes=0 minus the Server-Timing server time.",
            ),
        },
        Headline {
            label: "Jitter",
            value: d.jitter.value.map(format::ms),
            unit: "ms",
            color: Color::Jitter,
            hero: false,
            tip: stats_tip(
                &d.jitter,
                "ms",
                format::ms,
                "Stats are of the absolute differences.",
            ),
        },
        Headline {
            label: "Packet loss",
            value: d.loss.value.map(format::pct),
            unit: "%",
            color: Color::Loss,
            hero: false,
            tip: loss_tip,
        },
    ]
}

// ---- Breakdowns ----

fn retrans(label: &str, r: Option<Retrans>, how: &str) -> Kv {
    let v = r.map(|r| {
        let pct = if r.packets > 0 {
            r.retransmitted as f64 / r.packets as f64
        } else {
            0.0
        };
        format!(
            "{} of {} · {} %",
            format::count(r.retransmitted),
            format::count(r.packets),
            format::pct(pct)
        )
    });
    kv(label, v, how)
}

fn boxes(id: BoxId, d: &Derived) -> Item {
    Item::Boxes {
        id,
        rows: boxplot::rows(id, d).len().max(1),
    }
}

fn throughput_column(d: &Derived, dir: Direction) -> Column {
    let groups = &d.sizes[dir_idx(dir)];
    let mut items = vec![boxes(
        if dir == Direction::Download {
            BoxId::Download
        } else {
            BoxId::Upload
        },
        d,
    )];
    let transfers = d.sustained[dir_idx(dir)].map_or(0, |s| s.n);
    if transfers > 0 {
        items.push(Item::Kv(kv(
            "Transfers",
            Some(format::count(transfers as u64)),
            "Sustained transfers: every connection kept busy with back-to-back requests for a fixed time.",
        )));
    }
    let all: usize = groups.iter().filter_map(|g| g.summary.map(|s| s.n)).sum();
    let incomplete: usize = groups.iter().map(|g| g.incomplete).sum();
    if all > 0 || transfers == 0 {
        items.push(Item::Kv(kv(
            "Requests",
            (all > 0).then(|| format::count(all as u64)),
            "Requests run on their own, with a throughput value.",
        )));
    }
    if incomplete > 0 {
        items.push(Item::Kv(kv(
            "Cut at time limit",
            Some(format::count(incomplete as u64)),
            "Requests stopped at the per-request time limit; throughput uses the bytes moved so far.",
        )));
    }
    items.push(Item::Kv(kv(
        "TTFB median",
        d.ttfb_median[dir_idx(dir)].map(|t| format!("{} ms", format::ms(t))),
        match dir {
            Direction::Download => "Request start to first response byte.",
            Direction::Upload => {
                "Request start to the response, which the edge sends after the whole body."
            }
        },
    )));
    items.push(Item::Kv(match dir {
        Direction::Download => retrans(
            "Edge retransmits",
            d.retrans[0],
            "TCP segments Cloudflare's edge retransmitted while sending downloads (cfL4 Server-Timing counters, last report per connection).",
        ),
        Direction::Upload => retrans(
            "Retransmits",
            d.retrans[1],
            "TCP segments this machine retransmitted while uploading (kernel TCP counters per connection).",
        ),
    }));
    Column {
        title: dir.label(),
        items,
    }
}

fn latency_column(d: &Derived) -> Column {
    let mut items = vec![
        Item::Sub("HTTP · speed.cloudflare.com"),
        boxes(BoxId::Latency, d),
    ];
    if boxplot::shown(BoxId::Gateway, d) {
        items.push(Item::Sub("Gateway · ICMP echo"));
        items.push(boxes(BoxId::Gateway, d));
    }
    for load in Load::ALL {
        let j = d.jitter_by_load[load.as_u8() as usize];
        if j.is_none() && load != Load::Idle {
            continue;
        }
        items.push(Item::Kv(kv(
            &format!("Jitter · {}", load.label().to_lowercase()),
            j.map(|v| format!("{} ms", format::ms(v))),
            "Mean absolute difference between consecutive HTTP latency probes in this load state.",
        )));
    }
    items.push(Item::Kv(kv(
        "Edge TCP RTT",
        d.server_rtt
            .summary
            .map(|s| format!("{} ms", format::ms(s.median))),
        "Median smoothed RTT that Cloudflare's edge measured on the latency connections (cfL4 Server-Timing), unloaded.",
    )));
    Column {
        title: "Latency",
        items,
    }
}

fn loss_column(s: &Session, d: &Derived) -> Column {
    let mut items = Vec::new();
    for load in Load::ALL {
        let c = s.loss_counts[load.as_u8() as usize];
        if c.sent() == 0 && load != Load::Idle {
            continue;
        }
        let v = c.rate().map(|r| {
            format!(
                "{} % · {} of {}",
                format::pct(r),
                format::count(c.lost),
                format::count(c.sent())
            )
        });
        let mut how = String::from("UDP STUN probes with no answer within 3 s");
        if c.late > 0 {
            how.push_str(&format!("; {} answered later than that", c.late));
        }
        if c.duplicate > 0 {
            how.push_str(&format!("; {} duplicate answers", c.duplicate));
        }
        how.push('.');
        items.push(Item::Kv(kv(boxplot::load_label(load), v, &how)));
    }
    if boxplot::shown(BoxId::Udp, d) {
        items.push(Item::Sub("UDP round trip · STUN"));
        items.push(boxes(BoxId::Udp, d));
    }
    if s.mode == Some(RunMode::Continuous) && !d.windows.is_empty() {
        items.push(Item::Sub("Unloaded latency · jitter · loss"));
        for w in &d.windows {
            let v = w.latency.map(|l| {
                format!(
                    "{} ms · {} ms · {} %",
                    format::ms(l.median),
                    w.jitter.map_or(MISSING.into(), format::ms),
                    w.loss.rate().map_or(MISSING.into(), format::pct)
                )
            });
            let gw = w
                .gateway
                .map_or(MISSING.into(), |g| format!("{} ms", format::ms(g.median)));
            items.push(Item::Kv(kv(
                w.label,
                v,
                &format!(
                    "Median unloaded HTTP latency, jitter between consecutive unloaded probes, and UDP packet loss over the window. Gateway median over the same window: {gw}."
                ),
            )));
        }
    }
    Column {
        title: "Packet loss",
        items,
    }
}

// ---- Connection info ----

fn family(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(_) => "IPv4",
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some() => "IPv4",
        IpAddr::V6(_) => "IPv6",
    }
}

const PENDING: &str = "Measured when a test runs.";

fn row(label: &str, value: Option<String>, source: &str) -> Kv {
    let tip = if source.is_empty() {
        Tip::default()
    } else {
        Tip::default()
            .title(label.to_owned())
            .note(source.to_owned())
    };
    Kv {
        label: label.to_owned(),
        value,
        tip,
    }
}

fn sourced(label: &str, v: &Option<Sourced>) -> Kv {
    match v {
        Some(s) => row(
            label,
            Some(s.value.clone()),
            &format!("Source: {}", s.source),
        ),
        None => row(label, None, PENDING),
    }
}

fn info(s: &Session, d: &Derived) -> Vec<Section> {
    let e = &s.edge;
    let last_conn = s.conns.last();
    let mut out = Vec::new();

    let colo = e.colo.as_ref().map(|c| match &e.colo_city {
        Some(city) => format!("{} · {}", c.value, city.value),
        None => c.value.clone(),
    });
    let tls = last_conn
        .map(|c| c.tls_version.clone())
        .filter(|v| !v.is_empty());
    let cipher = last_conn
        .map(|c| c.cipher.clone())
        .filter(|v| !v.is_empty());
    out.push(Section {
        title: "Server",
        rows: vec![
            row(
                "Data center",
                colo,
                &e.colo.as_ref().map_or(PENDING.to_owned(), |c| {
                    format!("IATA code from {}", c.source)
                }),
            ),
            row(
                "Host",
                Some(cloudflare::HOST.to_owned()),
                "All HTTP tests run against this host.",
            ),
            row(
                "Address",
                last_conn.map(|c| c.remote.ip().to_string()),
                "Peer address of the most recent TCP connection.",
            ),
            sourced("HTTP", &e.http),
            row("TLS", tls, "Negotiated by this app's TLS client (rustls)."),
            row(
                "Cipher",
                cipher,
                "Negotiated by this app's TLS client (rustls).",
            ),
            sourced("Key exchange", &e.kex),
        ],
    });

    let network = e.asn.as_ref().map(|a| match &e.as_org {
        Some(o) => format!("{} · {}", a.value, o.value),
        None => a.value.clone(),
    });
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
    out.push(Section {
        title: "Client",
        rows: vec![
            sourced("Public IP", &e.client_ip),
            sourced("UDP mapping", &e.udp_mapped),
            row(
                "Network",
                network,
                &e.asn.as_ref().map_or(PENDING.to_owned(), |a| {
                    format!(
                        "Source: {}. The network name comes only from /meta, which answers with no data to clients that don't send the web test's Referer.",
                        a.source
                    )
                }),
            ),
            row(
                "Location",
                loc,
                &e.client_city
                    .as_ref()
                    .or(e.client_country.as_ref())
                    .map_or(PENDING.to_owned(), |c| {
                        format!(
                            "Cloudflare's geolocation of the public IP. Source: {}",
                            c.source
                        )
                    }),
            ),
            sourced("Coordinates", &e.client_coords),
            sourced("Time zone", &e.timezone),
            sourced("WARP", &e.warp),
        ],
    });

    let mut conn = vec![row(
        "IP version",
        last_conn.map(|c| family(c.local.ip()).to_owned()),
        "Address family of the most recent TCP connection.",
    )];
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
        conn.push(match res {
            Some(Ok(ms)) => row(
                label,
                Some(format!("{} ms", format::ms(ms))),
                &format!("TCP connect time to the first resolved address ({list})."),
            ),
            Some(Err(err)) => row(label, Some(UNAVAILABLE.into()), &err),
            None => row(label, None, PENDING),
        });
    }
    let iface = s.iface.as_ref();
    conn.extend([
        row(
            "DNS lookup",
            host.and_then(|h| h.dns_ms).map(|v| format!("{} ms", format::ms(v))),
            "System resolver (getaddrinfo), timed by this app.",
        ),
        row(
            "Local IP",
            iface.and_then(|i| i.local_ip).map(|i| i.to_string()),
            "Local address of the most recent TCP connection.",
        ),
        row(
            "Interface",
            iface.and_then(|i| i.name.clone()),
            "Interface holding the local address (getifaddrs).",
        ),
        row(
            "Type",
            iface.and_then(|i| i.kind).map(str::to_owned),
            "Wi-Fi from the system's wireless interface list, otherwise from the interface name.",
        ),
        row(
            "MTU",
            iface.and_then(|i| i.mtu).map(|m| m.to_string()),
            "Interface MTU in bytes from the link-layer address entry.",
        ),
        row(
            "Gateway",
            iface.and_then(|i| i.gateway).map(|g| g.to_string()),
            "Default IPv4 route of the interface (routing table).",
        ),
        row(
            "Gateway latency",
            d.gw_by_load[Load::Idle.as_u8() as usize]
                .summary
                .map(|s| format!("{} ms", format::ms(s.median))),
            "Median ICMP echo round trip to the gateway while the link is unloaded. macOS answers \"No route to host\" to apps without Local Network access (Privacy & Security settings).",
        ),
    ]);
    out.push(Section {
        title: "Connection",
        rows: conn,
    });

    match &s.wifi {
        Some(Some(w)) => {
            let loc_note = "macOS returns this only to apps with Location Services access; this app does not request it.";
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
            let snr = match (w.rssi_dbm, w.noise_dbm) {
                (Some(r), Some(n)) => Some(format!("{} dB", r - n)),
                _ => None,
            };
            out.push(Section {
                title: "Wi-Fi",
                rows: vec![
                    row(
                        "Network",
                        Some(w.ssid.clone().unwrap_or_else(|| UNAVAILABLE.into())),
                        if w.ssid.is_some() {
                            "CoreWLAN"
                        } else {
                            loc_note
                        },
                    ),
                    row(
                        "BSSID",
                        Some(w.bssid.clone().unwrap_or_else(|| UNAVAILABLE.into())),
                        if w.bssid.is_some() {
                            "CoreWLAN"
                        } else {
                            loc_note
                        },
                    ),
                    row(
                        "RSSI",
                        w.rssi_dbm.map(|v| format!("{v} dBm")),
                        "Received signal strength reported by CoreWLAN, sampled every 2 s.",
                    ),
                    row(
                        "Noise",
                        w.noise_dbm.map(|v| format!("{v} dBm")),
                        "Noise floor reported by CoreWLAN.",
                    ),
                    row("SNR", snr, "RSSI minus noise."),
                    row("Channel", ch, "CoreWLAN channel number, band and width."),
                    row(
                        "PHY mode",
                        w.phy_mode.map(str::to_owned),
                        "Active PHY mode reported by CoreWLAN.",
                    ),
                    row(
                        "Tx rate",
                        w.tx_rate_mbps.map(|r| format!("{} Mbps", format::sig3(r))),
                        "Current transmit rate reported by CoreWLAN.",
                    ),
                    row(
                        "Security",
                        w.security.map(str::to_owned),
                        "CoreWLAN security mode.",
                    ),
                ],
            });
        }
        Some(None) if iface.and_then(|i| i.kind) == Some("Wi-Fi") => out.push(Section {
            title: "Wi-Fi",
            rows: vec![row(
                "Wi-Fi",
                Some(UNAVAILABLE.into()),
                "This platform does not report Wi-Fi details yet.",
            )],
        }),
        _ => {}
    }
    out
}

// ---- Menu bar ----

fn tray(s: &Session, d: &Derived) -> Tray {
    let v = |x: Option<f64>, f: fn(f64) -> String, unit: &str| {
        x.map_or_else(|| MISSING.to_owned(), |x| format!("{} {unit}", f(x)))
    };
    Tray {
        title: d
            .latency
            .value
            .map(|x| format!("{} ms", format::ms(x)))
            .unwrap_or_default(),
        rows: vec![
            ("Download".into(), v(d.download.value, format::mbps, "Mbps")),
            ("Upload".into(), v(d.upload.value, format::mbps, "Mbps")),
            ("Latency".into(), v(d.latency.value, format::ms, "ms")),
            ("Jitter".into(), v(d.jitter.value, format::ms, "ms")),
            ("Packet loss".into(), v(d.loss.value, format::pct, "%")),
        ],
        status: status(s, None).text,
        one_shot_running: s.active() && s.mode == Some(RunMode::OneShot),
        continuous_running: s.active() && s.mode == Some(RunMode::Continuous),
        paused: s.state == RunState::Paused,
    }
}

fn legends() -> Legends {
    let l = |label, color, tick| Legend { label, color, tick };
    Legends {
        throughput: vec![
            l("Download", Color::Download, false),
            l("Upload", Color::Upload, false),
        ],
        latency: vec![
            l("Latency", Color::Latency, false),
            l("Gateway", GATEWAY, false),
            l("UDP probe lost", Color::Loss, true),
        ],
    }
}

/// `raw-speed-test-2026-10-19-140322.json`; `Z` marks a UTC time when the
/// local offset is unknown.
pub fn export_name(s: &Session) -> String {
    let stamp = s
        .wall_start
        .map(|w| {
            let clock = format::clock(w, false);
            let utc = clock.ends_with(" UTC");
            let digits: String = clock.chars().filter(char::is_ascii_digit).collect();
            format!("{}-{digits}{}", format::date(w), if utc { "Z" } else { "" })
        })
        .unwrap_or_else(|| "empty".into());
    format!("raw-speed-test-{stamp}.json")
}

/// The whole frame for one session snapshot.
pub fn build(
    s: &Session,
    d: &Derived,
    mode: RunMode,
    cfg: &Config,
    notice: Option<(&str, bool)>,
) -> Frame {
    let active = s.active();
    Frame {
        headline: headline(d),
        status: status(s, notice),
        progress: progress(s),
        elapsed: s
            .wall_start
            .map(|_| format::elapsed(s.now().unwrap_or(0.0))),
        controls: Controls {
            mode,
            mode_enabled: !active,
            active,
            paused: s.state == RunState::Paused,
            can_reset: active || s.has_data(),
            can_export: s.has_data(),
        },
        settings: settings(cfg, d),
        columns: vec![
            throughput_column(d, Direction::Download),
            throughput_column(d, Direction::Upload),
            latency_column(d),
            loss_column(s, d),
        ],
        info: info(s, d),
        rssi: super::chart::rssi::shown(s),
        log: LogCounts {
            requests: s.requests.len(),
            connections: s.conns.len(),
            errors: s.errors.len(),
        },
        tray: tray(s, d),
        legends: legends(),
        export_name: export_name(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::derived::Cache;

    #[test]
    fn empty_session_shows_dashes_not_numbers() {
        let s = Session::new();
        let mut c = Cache::default();
        c.update(&s);
        let f = build(&s, &c.derived, RunMode::OneShot, &Config::default(), None);
        assert!(f.headline.iter().all(|h| h.value.is_none()));
        assert_eq!(f.status.text, "Idle");
        assert!(!f.controls.active && !f.controls.can_export);
        assert!(f.tray.rows.iter().all(|(_, v)| v == MISSING));
        assert_eq!(f.export_name, "raw-speed-test-empty.json");
    }

    #[test]
    fn menu_bar_rows_match_headline_values() {
        let s = crate::app::session::synthetic(1800.0);
        let mut c = Cache::default();
        c.update(&s);
        let f = build(
            &s,
            &c.derived,
            RunMode::Continuous,
            &Config::default(),
            None,
        );
        for (h, (label, value)) in f.headline.iter().zip(&f.tray.rows) {
            assert_eq!(h.label, label);
            let v = h.value.as_ref().expect("synthetic data has every value");
            assert_eq!(&format!("{v} {}", h.unit), value);
        }
    }

    #[test]
    fn settings_round_trip() {
        let cfg = Config::default();
        for id in Setting::ALL {
            let opts = id.options();
            for i in 0..opts.len() {
                let c = id.apply(&cfg, i).expect("in range");
                assert_eq!(id.selected(&c), i, "{id:?} option {i}");
            }
            assert!(id.apply(&cfg, opts.len()).is_none());
        }
    }

    #[test]
    fn status_lines() {
        let mut s = Session::new();
        assert_eq!(status(&s, None).text, "Idle");
        assert_eq!(
            status(&s, Some(("Exported to x", false))).text,
            "Exported to x"
        );
        assert_eq!(
            status(&s, Some(("Export failed: y", true))).color,
            Color::Error
        );
        s.state = RunState::Running;
        s.phase = Some(Phase::Latency { done: 3, total: 20 });
        assert_eq!(status(&s, None).text, "Measuring latency · 3/20 probes");
        s.phase = Some(Phase::Connecting);
        assert_eq!(
            status(&s, None).text,
            format!("Connecting · {}", cloudflare::HOST)
        );
        s.phase = Some(Phase::BackingOff {
            until: 0.0,
            reason: "HTTP 429".into(),
        });
        let st = status(&s, None);
        assert!(st.text.starts_with("Retrying in") && st.text.ends_with("HTTP 429"));
        assert_eq!(st.color, Color::Warning);
    }
}
