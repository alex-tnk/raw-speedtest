//! Per-test breakdowns: throughput by payload size, latency by load,
//! packet loss, TCP retransmissions, rolling windows.

use eframe::egui::{Sense, Ui, vec2};

use super::charts::boxplot::{self, Caches, Plot, Row};
use super::text::{self, Text};
use super::widgets::{self, Tip, tooltip};
use super::{format, theme};
use crate::app::derived::{Derived, Retrans};
use crate::app::session::{Session, dir_idx};
use crate::engine::size_label;
use crate::model::{Direction, Load, ProbeOutcome, RunMode};

fn section(ui: &mut Ui, text: &Text, label: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), theme::S24), Sense::hover());
    text.paint(
        ui.painter(),
        eframe::egui::pos2(r.left(), r.center().y),
        eframe::egui::Align2::LEFT_CENTER,
        label,
        text::SECTION,
    );
}

fn tip_row(ui: &mut Ui, text: &Text, label: &str, value: Option<String>, how: &str) {
    let resp = widgets::kv_row(ui, text, label, value.as_deref(), text::BODY);
    if let Some(hp) = resp.hover_pos() {
        tooltip(
            ui,
            text,
            hp,
            &Tip::default().title(label.to_owned()).note(how.to_owned()),
        );
    }
}

fn retrans_row(ui: &mut Ui, text: &Text, label: &str, r: Option<Retrans>, how: &str) {
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
    tip_row(ui, text, label, v, how);
}

/// Throughput box plots for one direction.
fn throughput(
    ui: &mut Ui,
    text: &Text,
    s: &Session,
    d: &Derived,
    dir: Direction,
    caches: &mut Caches,
) {
    section(ui, text, dir.label());
    let groups = &d.sizes[dir_idx(dir)];
    let color = widgets::color_of_dir(dir);
    let rows: Vec<Row<'_>> = groups
        .iter()
        .map(|g| {
            let size = g.size;
            Row {
                label: size_label(size),
                summary: g.summary.map(|s| s.scaled(1e-6)),
                approx: false,
                color,
                points: Box::new(move || {
                    Box::new(
                        s.requests
                            .iter()
                            .enumerate()
                            .filter(move |(_, r)| r.dir == dir && r.size == size)
                            .filter_map(|(i, r)| r.bps.map(|b| (b / 1e6, i))),
                    )
                }),
            }
        })
        .collect();
    let n = rows.len().max(1);
    let h = n as f32 * theme::BOX_ROW + theme::AXIS_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let tip = |_row: usize, idx: usize| -> Tip {
        let Some(r) = s.requests.get(idx) else {
            return Tip::default();
        };
        let mut t = Tip::default().title(format!(
            "{} {} · sample {}/{}",
            size_label(r.size),
            r.dir.lower(),
            r.index,
            r.count
        ));
        if let Some(b) = r.bps {
            t.push_pair("Throughput", format!("{} Mbps", format::mbps(b)));
        }
        t.push_pair("Duration", format!("{} ms", format::ms(r.duration_ms)));
        if r.ttfb_ms.is_finite() {
            t.push_pair("TTFB", format!("{} ms", format::ms(r.ttfb_ms)));
        }
        if let Some(sv) = r.server_ms {
            t.push_pair("Server time", format!("{} ms", format::ms(sv)));
        }
        if let Some(w) = s.wall_at(r.t_start) {
            t.push_pair("Start", format::clock(w, true));
        }
        t.push_pair(
            "Connection",
            format!("#{}{}", r.conn, if r.new_conn { " · new" } else { "" }),
        );
        if !r.complete {
            t.push_pair(
                "Bytes",
                format!("{} of {}", format::count(r.bytes), format::count(r.size)),
            );
        }
        t
    };
    boxplot::draw(
        ui,
        text,
        rect,
        rows,
        &Plot {
            id: dir.lower(),
            version: s.version,
            unit: "Mbps",
            axis_label: &|v| format::tick(v),
            value_label: &|v| format::sig3(v),
            point_tip: &tip,
            note: "",
        },
        caches,
    );
    let all = d.sizes[dir_idx(dir)]
        .iter()
        .filter_map(|g| g.summary.map(|s| s.n))
        .sum::<usize>();
    let incomplete: usize = groups.iter().map(|g| g.incomplete).sum();
    tip_row(
        ui,
        text,
        "Requests",
        (all > 0).then(|| format::count(all as u64)),
        "Requests with a throughput value.",
    );
    if incomplete > 0 {
        tip_row(
            ui,
            text,
            "Cut at time limit",
            Some(format::count(incomplete as u64)),
            "Requests stopped at the per-request time limit; throughput uses the bytes moved so far.",
        );
    }
    let ttfb = crate::stats::summarize_iter(
        &mut Vec::new(),
        s.requests_in(dir)
            .map(|r| r.ttfb_ms)
            .filter(|v| v.is_finite()),
    );
    tip_row(
        ui,
        text,
        "TTFB median",
        ttfb.map(|t| format!("{} ms", format::ms(t.median))),
        match dir {
            Direction::Download => "Request start to first response byte.",
            Direction::Upload => {
                "Request start to the response, which the edge sends after the whole body."
            }
        },
    );
    match dir {
        Direction::Download => retrans_row(
            ui,
            text,
            "Edge retransmits",
            d.retrans[0],
            "TCP segments Cloudflare's edge retransmitted while sending downloads (cfL4 Server-Timing counters, last report per connection).",
        ),
        Direction::Upload => retrans_row(
            ui,
            text,
            "Retransmits",
            d.retrans[1],
            "TCP segments this machine retransmitted while uploading (kernel TCP counters per connection).",
        ),
    }
}

fn sublabel(ui: &mut Ui, text: &Text, label: &str) {
    let (r, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::S16 + theme::S4),
        Sense::hover(),
    );
    text.paint(
        ui.painter(),
        eframe::egui::pos2(r.left(), r.bottom()),
        eframe::egui::Align2::LEFT_BOTTOM,
        label,
        text::SMALL_2,
    );
}

fn load_label(load: Load) -> String {
    match load {
        Load::Idle => "Unloaded".into(),
        Load::Download => "Download".into(),
        Load::Upload => "Upload".into(),
    }
}

/// One metric split by load state.
struct ByLoad<'a> {
    id: &'static str,
    version: u64,
    stats: &'a [crate::app::derived::Stat; 3],
    color: eframe::egui::Color32,
    points: &'a dyn Fn(Load) -> Box<dyn Iterator<Item = (f64, usize)> + 'a>,
    tip: &'a dyn Fn(usize, usize) -> Tip,
    note: &'a str,
}

/// Box plot of one metric split by load state; empty loaded rows hidden.
fn by_load<'a>(ui: &mut Ui, text: &Text, m: &ByLoad<'a>, caches: &mut Caches) {
    let ByLoad {
        id,
        version,
        stats,
        color,
        points,
        tip,
        note,
    } = *m;
    let mut rows: Vec<Row<'a>> = Vec::new();
    for load in Load::ALL {
        let st = stats[load.as_u8() as usize];
        if st.summary.is_none() && load != Load::Idle {
            continue;
        }
        rows.push(Row {
            label: load_label(load),
            summary: st.summary,
            approx: st.approx,
            color,
            points: Box::new(move || points(load)),
        });
    }
    let h = rows.len().max(1) as f32 * theme::BOX_ROW + theme::AXIS_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    boxplot::draw(
        ui,
        text,
        rect,
        rows,
        &Plot {
            id,
            version,
            unit: "ms",
            axis_label: &|v| format::tick(v),
            value_label: &|v| format::ms(v),
            point_tip: tip,
            note,
        },
        caches,
    );
}

fn latency(ui: &mut Ui, text: &Text, s: &Session, d: &Derived, caches: &mut Caches) {
    section(ui, text, "Latency");
    sublabel(ui, text, "HTTP · speed.cloudflare.com");
    by_load(
        ui,
        text,
        &ByLoad {
            id: "latency-box",
            version: s.version,
            stats: &d.lat_by_load,
            color: theme::LATENCY,
            points: &|load| {
                Box::new(
                    s.latency
                        .raw
                        .iter()
                        .enumerate()
                        .filter(move |(_, x)| x.load == load)
                        .map(|(j, x)| (x.ms, j)),
                )
            },
            tip: &|_, idx| {
                let Some(x) = s.latency.raw.get(idx) else {
                    return Tip::default();
                };
                let mut t = Tip::default()
                    .title(format!("Latency probe · {}", x.load.label().to_lowercase()));
                t.push_pair("Latency", format!("{} ms", format::ms(x.ms)));
                t.push_pair("TTFB", format!("{} ms", format::ms(x.ttfb_ms)));
                if let Some(sv) = x.server_ms {
                    t.push_pair("Server time", format!("{} ms", format::ms(sv)));
                }
                if x.count > 0 {
                    t.push_pair("Probe", format!("{}/{}", x.index, x.count));
                }
                if let Some(w) = s.wall_at(x.t) {
                    t.push_pair("Time", format::clock(w, true));
                }
                t
            },
            note: "TTFB of GET __down?bytes=0 minus the Server-Timing server time.",
        },
        caches,
    );
    if d.gw_by_load.iter().any(|g| g.summary.is_some()) {
        sublabel(ui, text, "Gateway · ICMP echo");
        by_load(
            ui,
            text,
            &ByLoad {
                id: "gateway-box",
                version: s.version,
                stats: &d.gw_by_load,
                color: theme::GATEWAY,
                points: &|load| {
                    Box::new(
                        s.gateway
                            .raw
                            .iter()
                            .enumerate()
                            .filter(move |(_, g)| g.load == load)
                            .filter_map(|(j, g)| g.ms.map(|m| (m, j))),
                    )
                },
                tip: &|_, idx| {
                    let Some(g) = s.gateway.raw.get(idx) else {
                        return Tip::default();
                    };
                    let mut t = Tip::default()
                        .title(format!("Gateway ping · {}", g.load.label().to_lowercase()));
                    if let Some(m) = g.ms {
                        t.push_pair("Round trip", format!("{} ms", format::ms(m)));
                    }
                    if let Some(w) = s.wall_at(g.t) {
                        t.push_pair("Time", format::clock(w, true));
                    }
                    t
                },
                note: "ICMP echo round trip to the default gateway.",
            },
            caches,
        );
    }
    for load in Load::ALL {
        let j = d.jitter_by_load[load.as_u8() as usize];
        if j.is_none() && load != Load::Idle {
            continue;
        }
        tip_row(
            ui,
            text,
            &format!("Jitter · {}", load.label().to_lowercase()),
            j.map(|v| format!("{} ms", format::ms(v))),
            "Mean absolute difference between consecutive HTTP latency probes in this load state.",
        );
    }
    tip_row(
        ui,
        text,
        "Edge TCP RTT",
        d.server_rtt
            .summary
            .map(|s| format!("{} ms", format::ms(s.median))),
        "Median smoothed RTT that Cloudflare's edge measured on the latency connections (cfL4 Server-Timing), unloaded.",
    );
}

fn loss(ui: &mut Ui, text: &Text, s: &Session, d: &Derived, caches: &mut Caches) {
    section(ui, text, "Packet loss");
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
        tip_row(ui, text, &load_label(load), v, &how);
    }
    if d.udp_rtt_by_load.iter().any(|g| g.summary.is_some()) {
        sublabel(ui, text, "UDP round trip · STUN");
        by_load(
            ui,
            text,
            &ByLoad {
                id: "udp-box",
                version: s.version,
                stats: &d.udp_rtt_by_load,
                color: theme::LATENCY,
                points: &|load| {
                    Box::new(
                        s.loss
                            .raw
                            .iter()
                            .enumerate()
                            .filter(move |(_, p)| {
                                p.load == load && p.outcome == ProbeOutcome::Received
                            })
                            .filter_map(|(j, p)| p.rtt_ms.map(|r| (r, j))),
                    )
                },
                tip: &|_, idx| {
                    let Some(p) = s.loss.raw.get(idx) else {
                        return Tip::default();
                    };
                    let mut t = Tip::default().title(format!(
                        "UDP probe {} · {}",
                        p.seq,
                        p.load.label().to_lowercase()
                    ));
                    if let Some(r) = p.rtt_ms {
                        t.push_pair("Round trip", format!("{} ms", format::ms(r)));
                    }
                    if let Some(w) = s.wall_at(p.t) {
                        t.push_pair("Sent", format::clock(w, true));
                    }
                    t
                },
                note: "Round trip of STUN Binding requests to stun.cloudflare.com:3478.",
            },
            caches,
        );
    }
    if s.mode == Some(RunMode::Continuous) && !d.windows.is_empty() {
        sublabel(ui, text, "Unloaded latency · jitter · loss");
        for w in &d.windows {
            let v = w.latency.map(|l| {
                format!(
                    "{} ms · {} ms · {} %",
                    format::ms(l.median),
                    w.jitter.map_or("—".into(), format::ms),
                    w.loss.rate().map_or("—".into(), format::pct)
                )
            });
            let gw = w
                .gateway
                .map_or("—".into(), |g| format!("{} ms", format::ms(g.median)));
            tip_row(
                ui,
                text,
                w.label,
                v,
                &format!(
                    "Median unloaded HTTP latency, jitter between consecutive unloaded probes, and UDP packet loss over the window. Gateway median over the same window: {gw}."
                ),
            );
        }
    }
}

/// Four columns of breakdowns.
pub fn show(ui: &mut Ui, text: &Text, s: &Session, d: &Derived, caches: &mut Caches) {
    let gap = theme::S12;
    let w = ((ui.available_width() - 3.0 * gap) / 4.0).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for col in 0..4 {
            ui.allocate_ui_with_layout(
                vec2(w, 0.0),
                eframe::egui::Layout::top_down(eframe::egui::Align::Min),
                |ui| {
                    widgets::frame().show(ui, |ui| {
                        ui.set_width(w - 2.0 * theme::S12);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        match col {
                            0 => throughput(ui, text, s, d, Direction::Download, caches),
                            1 => throughput(ui, text, s, d, Direction::Upload, caches),
                            2 => latency(ui, text, s, d, caches),
                            _ => loss(ui, text, s, d, caches),
                        }
                    });
                },
            );
        }
    });
}
