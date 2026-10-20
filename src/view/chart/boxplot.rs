//! Horizontal box plots: min, p25, median, p75, max, plus every point.

use super::{Hover, Transform, nice_max, nice_ticks};
use crate::app::derived::{Derived, Stat};
use crate::app::session::{Session, dir_idx};
use crate::engine::size_label;
use crate::model::{Direction, Load, ProbeOutcome};
use crate::stats::Summary;
use crate::view::format;
use crate::view::palette::{self, Color, GATEWAY};
use crate::view::scene::{HAlign, Point, Rect, Scene, TextSize, VAlign, pt};
use crate::view::tip::Tip;

/// The box plots in the breakdown columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BoxId {
    /// Download throughput by payload size.
    Download = 0,
    /// Upload throughput by payload size.
    Upload = 1,
    /// HTTP latency by load.
    Latency = 2,
    /// Gateway ICMP round trip by load.
    Gateway = 3,
    /// UDP STUN round trip by load.
    Udp = 4,
}

impl BoxId {
    pub const ALL: [BoxId; 5] = [
        BoxId::Download,
        BoxId::Upload,
        BoxId::Latency,
        BoxId::Gateway,
        BoxId::Udp,
    ];

    pub fn from_u8(v: u8) -> Option<BoxId> {
        BoxId::ALL.get(usize::from(v)).copied()
    }
}

#[derive(Clone, Copy)]
enum Key {
    Size(u64),
    Load(Load),
}

pub struct Row {
    pub label: String,
    pub summary: Option<Summary>,
    pub approx: bool,
    pub color: Color,
    key: Key,
}

struct Spec {
    unit: &'static str,
    axis: fn(f64) -> String,
    value: fn(f64) -> String,
    note: &'static str,
}

fn spec(id: BoxId) -> Spec {
    match id {
        BoxId::Download | BoxId::Upload => Spec {
            unit: "Mbps",
            axis: format::tick,
            value: format::sig3,
            note: "",
        },
        BoxId::Latency => Spec {
            unit: "ms",
            axis: format::tick,
            value: format::ms,
            note: "TTFB of GET __down?bytes=0 minus the Server-Timing server time.",
        },
        BoxId::Gateway => Spec {
            unit: "ms",
            axis: format::tick,
            value: format::ms,
            note: "ICMP echo round trip to the default gateway.",
        },
        BoxId::Udp => Spec {
            unit: "ms",
            axis: format::tick,
            value: format::ms,
            note: "Round trip of STUN Binding requests to stun.cloudflare.com:3478.",
        },
    }
}

pub fn load_label(load: Load) -> &'static str {
    match load {
        Load::Idle => "Unloaded",
        Load::Download => "Download",
        Load::Upload => "Upload",
    }
}

fn by_load(stats: &[Stat; 3], color: Color) -> Vec<Row> {
    Load::ALL
        .into_iter()
        .filter_map(|load| {
            let st = stats[load.as_u8() as usize];
            // Loaded rows only once they have data.
            (st.summary.is_some() || load == Load::Idle).then(|| Row {
                label: load_label(load).to_owned(),
                summary: st.summary,
                approx: st.approx,
                color,
                key: Key::Load(load),
            })
        })
        .collect()
}

/// The rows of a box plot.
pub fn rows(id: BoxId, d: &Derived) -> Vec<Row> {
    match id {
        BoxId::Download | BoxId::Upload => {
            let dir = if id == BoxId::Download {
                Direction::Download
            } else {
                Direction::Upload
            };
            d.sizes[dir_idx(dir)]
                .iter()
                .map(|g| Row {
                    label: size_label(g.size),
                    summary: g.summary.map(|s| s.scaled(1e-6)),
                    approx: false,
                    color: super::series::dir_color(dir),
                    key: Key::Size(g.size),
                })
                .collect()
        }
        BoxId::Latency => by_load(&d.lat_by_load, Color::Latency),
        BoxId::Gateway => by_load(&d.gw_by_load, GATEWAY),
        BoxId::Udp => by_load(&d.udp_rtt_by_load, Color::Latency),
    }
}

/// Whether the box plot has anything to show (the gateway and UDP plots
/// appear only once measured).
pub fn shown(id: BoxId, d: &Derived) -> bool {
    match id {
        BoxId::Gateway => d.gw_by_load.iter().any(|g| g.summary.is_some()),
        BoxId::Udp => d.udp_rtt_by_load.iter().any(|g| g.summary.is_some()),
        _ => true,
    }
}

/// Height for `rows` rows plus the axis.
pub fn height(rows: usize) -> f32 {
    rows.max(1) as f32 * palette::BOX_ROW + palette::AXIS_HEIGHT
}

/// Calls `f(value, payload index)` for every point of a row.
fn for_points(id: BoxId, s: &Session, key: Key, mut f: impl FnMut(f64, usize)) {
    match (id, key) {
        (BoxId::Download | BoxId::Upload, Key::Size(size)) => {
            let dir = if id == BoxId::Download {
                Direction::Download
            } else {
                Direction::Upload
            };
            for (i, r) in s.requests.iter().enumerate() {
                if r.dir == dir
                    && r.size == size
                    && let Some(b) = r.bps
                {
                    f(b / 1e6, i);
                }
            }
        }
        (BoxId::Latency, Key::Load(load)) => {
            for (j, x) in s.latency.raw.iter().enumerate() {
                if x.load == load {
                    f(x.ms, j);
                }
            }
        }
        (BoxId::Gateway, Key::Load(load)) => {
            for (j, g) in s.gateway.raw.iter().enumerate() {
                if g.load == load
                    && let Some(m) = g.ms
                {
                    f(m, j);
                }
            }
        }
        (BoxId::Udp, Key::Load(load)) => {
            for (j, p) in s.loss.raw.iter().enumerate() {
                if p.load == load
                    && p.outcome == ProbeOutcome::Received
                    && let Some(r) = p.rtt_ms
                {
                    f(r, j);
                }
            }
        }
        _ => {}
    }
}

fn point_tip(id: BoxId, s: &Session, idx: usize) -> Tip {
    let clock = |t: f64| s.wall_at(t).map(|w| format::clock(w, true));
    match id {
        BoxId::Download | BoxId::Upload => {
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
            if let Some(w) = clock(r.t_start) {
                t.push_pair("Start", w);
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
        }
        BoxId::Latency => {
            let Some(x) = s.latency.raw.get(idx) else {
                return Tip::default();
            };
            let mut t =
                Tip::default().title(format!("Latency probe · {}", x.load.label().to_lowercase()));
            t.push_pair("Latency", format!("{} ms", format::ms(x.ms)));
            t.push_pair("TTFB", format!("{} ms", format::ms(x.ttfb_ms)));
            if let Some(sv) = x.server_ms {
                t.push_pair("Server time", format!("{} ms", format::ms(sv)));
            }
            if x.count > 0 {
                t.push_pair("Probe", format!("{}/{}", x.index, x.count));
            }
            if let Some(w) = clock(x.t) {
                t.push_pair("Time", w);
            }
            t
        }
        BoxId::Gateway => {
            let Some(g) = s.gateway.raw.get(idx) else {
                return Tip::default();
            };
            let mut t =
                Tip::default().title(format!("Gateway ping · {}", g.load.label().to_lowercase()));
            if let Some(m) = g.ms {
                t.push_pair("Round trip", format!("{} ms", format::ms(m)));
            }
            if let Some(w) = clock(g.t) {
                t.push_pair("Time", w);
            }
            t
        }
        BoxId::Udp => {
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
            if let Some(w) = clock(p.t) {
                t.push_pair("Sent", w);
            }
            t
        }
    }
}

fn summary_tip(id: BoxId, row: &Row, s: &Summary) -> Tip {
    let sp = spec(id);
    let v = |x: f64| format!("{} {}", (sp.value)(x), sp.unit);
    let mut t = Tip::default()
        .title(row.label.clone())
        .pair("Min", v(s.min))
        .pair("p25", v(s.p25))
        .pair("Median", v(s.median))
        .pair("p75", v(s.p75))
        .pair("Max", v(s.max))
        .pair("Samples", format::count(s.n as u64));
    if !sp.note.is_empty() {
        t = t.note(sp.note);
    }
    if row.approx {
        t = t.note("Percentiles from the whole-run histogram, within 0.5 %.");
    }
    t
}

struct Layout {
    area: Rect,
    tf: Transform,
    row_h: f32,
    half: f32,
    vmax: f64,
}

impl Layout {
    fn new(rows: &[Row], w: f32, h: f32) -> Layout {
        let area = Rect {
            x0: palette::BOX_LABEL,
            y0: 0.0,
            x1: (w - palette::S8).max(palette::BOX_LABEL + 1.0),
            y1: (h - palette::AXIS_HEIGHT).max(1.0),
        };
        let vmax = rows
            .iter()
            .filter_map(|r| r.summary.map(|s| s.max))
            .fold(0.0, f64::max);
        let tf = Transform {
            rect: area,
            t0: 0.0,
            t1: nice_max(vmax),
            v0: 0.0,
            v1: 1.0,
        };
        let n = rows.len().max(1);
        let row_h = (area.height() / n as f32).min(palette::BOX_ROW * 1.5);
        let half = (row_h * 0.3).min(palette::S8);
        Layout {
            area,
            tf,
            row_h,
            half,
            vmax,
        }
    }

    fn cy(&self, ri: usize) -> f32 {
        self.area.y0 + self.row_h * (ri as f32 + 0.5)
    }
}

/// Draws the box plot `id` into `sc` for a chart of `w`×`h` points.
pub fn scene(
    id: BoxId,
    s: &Session,
    d: &Derived,
    w: f32,
    h: f32,
    sc: &mut Scene,
    bits: &mut Vec<u64>,
) {
    let rows = rows(id, d);
    let l = Layout::new(&rows, w, h);
    let (area, tf) = (l.area, l.tf);
    let sp = spec(id);
    sc.clear(area);
    if l.vmax > 0.0 {
        let mut ticks = Vec::new();
        nice_ticks(
            0.0,
            tf.t1,
            ((area.width() / palette::TICK_SPACING_BOX) as usize).max(2),
            &mut ticks,
        );
        let last = ticks.len().saturating_sub(1);
        for (i, &v) in ticks.iter().enumerate() {
            let x = tf.x(v).round() + 0.5;
            sc.segment(
                pt(x, area.y0),
                pt(x, area.y1),
                palette::STROKE,
                Color::Grid,
                1.0,
            );
            let h = if i == last && x > area.x1 - palette::S16 {
                HAlign::End
            } else {
                HAlign::Center
            };
            sc.text(
                pt(x, area.y1 + palette::S4),
                &(sp.axis)(v),
                TextSize::Small,
                Color::Text2,
                h,
                VAlign::Top,
            );
        }
        sc.text(
            pt(0.0, area.y1 + palette::S4),
            sp.unit,
            TextSize::Small,
            Color::Text2,
            HAlign::Start,
            VAlign::Top,
        );
    }
    let mut dots = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let y = l.cy(ri);
        sc.text(
            pt(0.0, y),
            &row.label,
            TextSize::Body,
            Color::Text2,
            HAlign::Start,
            VAlign::Center,
        );
        let Some(st) = row.summary else {
            sc.text(
                pt(area.x0, y),
                "—",
                TextSize::Body,
                Color::Text2,
                HAlign::Start,
                VAlign::Center,
            );
            continue;
        };
        let (xmin, x25, x50, x75, xmax) = (
            tf.x(st.min),
            tf.x(st.p25),
            tf.x(st.median),
            tf.x(st.p75),
            tf.x(st.max),
        );
        let (c, w1, half) = (row.color, palette::STROKE, l.half);
        sc.segment(pt(xmin, y), pt(x25, y), w1, c, 1.0);
        sc.segment(pt(x75, y), pt(xmax, y), w1, c, 1.0);
        for x in [xmin, xmax] {
            sc.segment(pt(x, y - half * 0.6), pt(x, y + half * 0.6), w1, c, 1.0);
        }
        let x75 = x75.max(x25 + 1.0);
        let bx = Rect {
            x0: x25,
            y0: y - half,
            x1: x75,
            y1: y + half,
        };
        sc.fill(bx, c, palette::FILL_ALPHA);
        sc.outline(bx, c, 1.0);
        sc.segment(
            pt(x50, y - half),
            pt(x50, y + half),
            palette::DATA_LINE,
            c,
            1.0,
        );
        // Every point, one dot per occupied pixel column.
        bits.clear();
        bits.resize(area.width() as usize / 64 + 2, 0);
        dots.clear();
        for_points(id, s, row.key, |v, _| {
            let x = tf.x(v);
            let col = (x - area.x0).max(0.0) as usize;
            let (wd, bit) = (col / 64, col % 64);
            if wd >= bits.len() || bits[wd] & (1 << bit) != 0 {
                return;
            }
            bits[wd] |= 1 << bit;
            dots.push(pt(x, y));
        });
        sc.dots(&dots, palette::POINT_RADIUS * 0.75, c, 1.0);
    }
}

/// Hover: the nearest point in the hovered row, else the row's box stats.
pub fn hover(id: BoxId, s: &Session, d: &Derived, w: f32, h: f32, hp: Point) -> Hover {
    let rows = rows(id, d);
    let l = Layout::new(&rows, w, h);
    let ri = ((hp.y - l.area.y0) / l.row_h).floor();
    if ri < 0.0 || ri as usize >= rows.len() {
        return Hover::default();
    }
    let ri = ri as usize;
    let row = &rows[ri];
    let Some(st) = row.summary else {
        return Hover::default();
    };
    let y = l.cy(ri);
    if (hp.y - y).abs() > l.half + 2.0 {
        return Hover::default();
    }
    let mut best: Option<(f32, usize, Point)> = None;
    for_points(id, s, row.key, |v, idx| {
        let x = l.tf.x(v);
        let dx = (x - hp.x).abs();
        if dx <= palette::HOVER_PX && best.is_none_or(|b| dx < b.0) {
            best = Some((dx, idx, pt(x, y)));
        }
    });
    if let Some((_, idx, p)) = best {
        Hover {
            crosshair: None,
            highlight: Some((p, row.color)),
            tip: Some(point_tip(id, s, idx)),
            anchor: p,
        }
    } else if hp.x >= l.tf.x(st.min) - 2.0 && hp.x <= l.tf.x(st.max) + 2.0 {
        Hover {
            tip: Some(summary_tip(id, row, &st)),
            anchor: pt(hp.x, y - l.half),
            ..Hover::default()
        }
    } else {
        Hover::default()
    }
}
