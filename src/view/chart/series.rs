//! Throughput and latency over time, and the whole-run overview strip.

use std::time::{Duration, SystemTime};

use super::{
    Decimator, Hover, TimeView, Transform, nice_max, nice_ticks, percentile_labels, robust_top,
    segments, time_axis, time_plot, value_axis,
};
use crate::app::session::{RunState, Session, Transfer};
use crate::engine::size_label;
use crate::model::{Direction, Phase, ProbeOutcome};
use crate::stats::{Bucket, Sample, Series as SampleSeries, Tier};
use crate::view::format;
use crate::view::palette::{self, Color, GATEWAY};
use crate::view::scene::{HAlign, Point, Rect, Scene, TextSize, VAlign, pt};
use crate::view::tip::Tip;

/// What the time-series charts read.
pub struct Inputs<'a> {
    pub s: &'a Session,
    pub wall0: SystemTime,
    pub run_end: f64,
}

/// A value recomputed only when its key changes.
#[derive(Default)]
struct Memo<T> {
    key: Option<(u64, u64, u64)>,
    val: T,
}

impl<T: Copy> Memo<T> {
    fn get(&mut self, key: (u64, u64, u64), f: impl FnOnce() -> T) -> T {
        if self.key != Some(key) {
            self.val = f();
            self.key = Some(key);
        }
        self.val
    }
}

type Pct = Option<(f64, f64)>;

/// What the latency chart's axis is computed from.
#[derive(Clone, Copy, Default)]
struct LatStats {
    /// Highest latency or gateway sample in view.
    max: f64,
    /// Top of the value axis; below `max` when rare spikes are cut.
    top: f64,
    /// (p50, p90) of latency in view.
    pct: Pct,
}

/// What the throughput chart's axis and mode are computed from.
#[derive(Clone, Copy, Default)]
struct TputStats {
    /// Highest 50 ms bin, its robust top, and highest transfer throughput
    /// in view.
    bins_max: f64,
    bins_top: f64,
    req_max: f64,
    /// Mean request duration in view, seconds.
    mean_req_secs: f64,
    /// Per direction, (p50, p90) of transfer throughput in view.
    pct: [Pct; 2],
}

/// Axis maxima and percentiles are kept until the data or the view
/// settings change. While following live data the range creeps forward
/// with the clock between data updates; the statistics catch up at the next
/// update instead of being recomputed on every frame and hover.
fn stats_key(s: &Session, view: &TimeView) -> (u64, u64, u64) {
    (
        s.version,
        view.span.map_or(u64::MAX, f64::to_bits),
        view.end.map_or(u64::MAX, f64::to_bits),
    )
}

/// Scratch buffers and per-view statistics for the time-series charts.
#[derive(Default)]
pub struct Charts {
    tput_stats: Memo<TputStats>,
    /// Axis maximum and (p50, p90) of latency for the view.
    lat_stats: Memo<LatStats>,
    cols: Vec<Col>,
    pts: Vec<Point>,
    /// Second point buffer for aggregates drawn while `pts` is in use.
    aux: Vec<Point>,
    breaks: Vec<usize>,
    ticks: Vec<f64>,
    vals: Vec<f64>,
}

fn dir_of(i: usize) -> Direction {
    if i == 0 {
        Direction::Download
    } else {
        Direction::Upload
    }
}

pub fn dir_color(d: Direction) -> Color {
    match d {
        Direction::Download => Color::Download,
        Direction::Upload => Color::Upload,
    }
}

/// Visible raw index range, or whole series.
fn visible<T: Sample>(series: &SampleSeries<T>, t0: f64, t1: f64) -> (usize, usize) {
    let (a, b) = series.raw_range(t0, t1);
    // One sample of margin on each side so lines run to the plot edges.
    (a.saturating_sub(1), (b + 1).min(series.raw.len()))
}

/// The tier to draw where raw samples have been evicted, if any part of
/// `[t0, raw_start)` is visible.
fn aged_tier<T: Sample>(series: &SampleSeries<T>, t0: f64, t1: f64) -> Option<(&Tier, f64)> {
    if series.raw_complete() {
        return None;
    }
    let raw_start = series.raw_start()?;
    if t0 >= raw_start {
        return None;
    }
    let tier = if t1 - t0 > 3.0 * 86_400.0 {
        &series.hour
    } else {
        &series.minute
    };
    Some((tier, raw_start))
}

/// Envelope (min–max bar) and mean line for aggregated buckets.
fn draw_buckets(
    sc: &mut Scene,
    tier: &Tier,
    until: f64,
    tf: &Transform,
    color: Color,
    pts: &mut Vec<Point>,
) {
    pts.clear();
    let mut i = tier.first_ending_after(tf.t0);
    while let Some(bk) = tier.get(i) {
        if bk.t0 > tf.t1 || bk.t0 >= until {
            break;
        }
        if let Some(mean) = bk.mean() {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            sc.segment(
                pt(x, tf.yc(bk.min)),
                pt(x, tf.yc(bk.max)),
                palette::STROKE,
                color,
                palette::ENVELOPE_ALPHA,
            );
            pts.push(pt(x, tf.yc(mean)));
        }
        i += 1;
    }
    sc.line(pts, palette::DATA_LINE, color, 1.0);
}

/// True when the view holds far more raw samples than points across. The
/// chart then draws from per-minute min/max aggregates: at that zoom the M4
/// envelope is the same, and building it doesn't walk every raw sample.
fn coarse(visible_samples: usize, plot: Rect) -> bool {
    visible_samples as f32 > 8.0 * plot.width()
}

/// The finest tier that still covers `t0`.
fn tier_for<T: Sample>(series: &SampleSeries<T>, t0: f64) -> &Tier {
    match series.minute.get(0) {
        Some(b) if b.t0 <= t0 => &series.minute,
        _ => &series.hour,
    }
}

/// Loss ticks: one per pixel column holding a lost probe. Zoomed far out,
/// read per-minute miss counts instead of every probe.
fn loss_ticks(sc: &mut Scene, s: &Session, tf: &Transform, y0: f32, y1: f32, width: f32) {
    let (pa, pb) = s.loss.raw_range(tf.t0, tf.t1);
    let mut last_x = f32::NEG_INFINITY;
    let mut tick = |sc: &mut Scene, t: f64| {
        let x = tf.x(t).round() + 0.5;
        if x - last_x >= 1.0 {
            sc.segment(pt(x, y0), pt(x, y1), width, Color::Loss, 1.0);
            last_x = x;
        }
    };
    if coarse(pb - pa, tf.rect) || pa == pb && !s.loss.raw_complete() {
        let tier = tier_for(&s.loss, tf.t0);
        let mut i = tier.first_ending_after(tf.t0);
        while let Some(bk) = tier.get(i) {
            if bk.t0 > tf.t1 {
                break;
            }
            if bk.misses > 0 {
                tick(sc, bk.t0 + tier.width / 2.0);
            }
            i += 1;
        }
    } else {
        for p in s.loss.raw.range(pa, pb) {
            if p.outcome == ProbeOutcome::Lost {
                tick(sc, p.t);
            }
        }
    }
}

/// Feed each bucket's min and max to the decimator; empty buckets break
/// the line.
fn feed_buckets(dec: &mut Decimator<'_>, tier: &Tier, tf: &Transform) {
    let mut i = tier.first_ending_after(tf.t0);
    while let Some(bk) = tier.get(i) {
        if bk.t0 > tf.t1 {
            break;
        }
        if bk.n > 0 {
            let t = bk.t0 + tier.width / 2.0;
            dec.push(tf.pos(t, bk.min));
            dec.push(tf.pos(t, bk.max));
        } else {
            dec.gap();
        }
        i += 1;
    }
}

/// Transfers kept beyond each edge of the view, so trend lines run to the
/// plot edges.
const TREND_MARGIN: usize = 4;

/// Feeds `dec` a line through each set's median throughput in direction
/// `dir` (transfers of one cycle run back to back) and puts every
/// transfer's point in `dots`.
fn transfer_trend(
    dec: &mut Decimator<'_>,
    dots: &mut Vec<Point>,
    vals: &mut Vec<f64>,
    s: &Session,
    dir: Direction,
    tf: &Transform,
) {
    let gap_limit = transfer_gap(s);
    let mut prev: Option<f64> = None;
    let mut set: Option<(f64, f64)> = None;
    vals.clear();
    let mut flush = |dec: &mut Decimator<'_>, vals: &mut Vec<f64>, set: (f64, f64)| {
        if let Some(sm) = crate::stats::summarize(vals) {
            let t = (set.0 + set.1) / 2.0;
            if prev.is_some_and(|p| t - p > gap_limit) {
                dec.gap();
            }
            dec.push(tf.pos(t, sm.median));
            prev = Some(t);
        }
        vals.clear();
    };
    for x in s
        .transfers(tf.t0, tf.t1, TREND_MARGIN)
        .filter(|x| x.dir() == dir)
    {
        let Some(b) = x.bps() else { continue };
        match &mut set {
            Some(st) if x.t_start() - st.1 <= SET_GAP => st.1 = x.t_end(),
            _ => {
                if let Some(st) = set {
                    flush(dec, vals, st);
                }
                set = Some((x.t_start(), x.t_end()));
            }
        }
        vals.push(b);
        dots.push(tf.pos(mid(&x), b));
    }
    if let Some(st) = set {
        flush(dec, vals, st);
    }
}

fn mid(x: &Transfer<'_>) -> f64 {
    (x.t_start() + x.t_end()) / 2.0
}

/// How many bins a hover looks past on each side for one that is drawn.
const BIN_SEARCH: usize = 256;

/// Transfers starting within this of the previous one's end belong to the
/// same set (one continuous cycle, or one one-shot test).
const SET_GAP: f64 = 30.0;

/// Transfers further apart than this aren't joined by the trend line:
/// a few continuous cycles missed (stopped, paused, failing).
fn transfer_gap(s: &Session) -> f64 {
    (3.0 * s.config.continuous.cycle_interval.as_secs_f64()).max(120.0)
}

/// Whether transfer `x` is too narrow at this zoom to show its bins.
fn too_narrow(tf: &Transform, x: &Transfer<'_>) -> bool {
    tf.x(x.t_end()) - tf.x(x.t_start()) < palette::REQUEST_MIN_WIDTH
}

/// Title, values and how it's measured, for transfer `x`.
fn transfer_tip(inp: &Inputs<'_>, x: &Transfer<'_>, zoomed_out: bool) -> Tip {
    let zoom = if zoomed_out {
        " Zoom in to see its 50 ms bins."
    } else {
        ""
    };
    match x {
        Transfer::Sustained(r) => {
            let mut tip =
                Tip::default().title(format!("{} · {} connections", r.dir.label(), r.streams));
            if let Some(b) = r.bps {
                tip.push_pair("Throughput", format!("{} Mbps", format::mbps(b)));
            }
            tip.push_pair("Start", wall(inp, r.t_start));
            tip.push_pair("Length", format!("{} s", format::sig3(r.t_end - r.t_start)));
            tip.push_pair("Data", format!("{} MB", format::sig3(r.bytes as f64 / 1e6)));
            tip.push_pair("Requests", format::count(u64::from(r.requests)));
            if !r.complete {
                tip.push_pair("Ended", "early".to_owned());
            }
            tip.note(format!(
                "Bytes {} on every connection after the first {} s (TCP ramp-up), over that time.{zoom}",
                match r.dir {
                    Direction::Download => "received",
                    Direction::Upload => "acknowledged by the server",
                },
                format::sig3(r.t_steady - r.t_start)
            ))
        }
        Transfer::Single(r) => {
            let mut tip = Tip::default().title(format!(
                "{} {} · sample {}/{}",
                size_label(r.size),
                r.dir.lower(),
                r.index,
                r.count
            ));
            if let Some(b) = r.bps {
                tip.push_pair("Throughput", format!("{} Mbps", format::mbps(b)));
            }
            tip.push_pair("Start", wall(inp, r.t_start));
            tip.push_pair("Duration", format!("{} ms", format::ms(r.duration_ms)));
            tip.push_pair("TTFB", format!("{} ms", format::ms(r.ttfb_ms)));
            tip.note(format!(
                "{}{zoom}",
                match r.dir {
                    Direction::Download => {
                        "Bits received over TTFB minus server time plus transfer time."
                    }
                    Direction::Upload => "Bits sent over request start to first response byte.",
                }
            ))
        }
    }
}

/// The transfer point nearest the pointer among those `only` admits, with
/// its distance score; `None` if none is near.
fn transfer_hover(
    inp: &Inputs<'_>,
    tf: &Transform,
    hp: Point,
    only: impl Fn(&Transfer<'_>) -> bool,
) -> Option<(Hover, f32)> {
    let near = inp
        .s
        .transfers(tf.t0, tf.t1, TREND_MARGIN)
        .filter(|x| only(x))
        .filter_map(|x| {
            let p = tf.pos(mid(&x), x.bps()?);
            Some((x, p, (p.x - hp.x).abs() + 0.25 * (p.y - hp.y).abs()))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let (x, p, d) = near.filter(|(_, p, _)| (p.x - hp.x).abs() <= palette::SNAP_PX)?;
    let hv = Hover {
        crosshair: Some(p.x),
        highlight: Some((p, dir_color(x.dir()))),
        tip: Some(transfer_tip(inp, &x, true)),
        anchor: p,
    };
    Some((hv, d))
}

/// One column of dense samples: their range and median (aggregates: mean).
#[derive(Clone, Copy, Debug)]
struct Col {
    /// Centre and width in points.
    x: f32,
    w: f32,
    min: f64,
    max: f64,
    /// Interquartile range, when the column holds raw samples.
    iqr: Option<(f64, f64)>,
    mid: f64,
    /// The line breaks before this column (a gap in the samples).
    gap: bool,
}

/// Summarises `(t, value)` samples per `palette::COLUMN`-wide column; a
/// `None` value or a pause longer than `gap_limit` breaks the line.
fn columns_raw(
    out: &mut Vec<Col>,
    vals: &mut Vec<f64>,
    tf: &Transform,
    gap_limit: f64,
    items: impl Iterator<Item = (f64, Option<f64>)>,
) {
    out.clear();
    vals.clear();
    let w = palette::COLUMN;
    let col_of = |t: f64| ((tf.x(t) - tf.rect.x0) / w).floor() as i64;
    let mut cur: Option<i64> = None;
    let mut gap = false;
    let mut prev_t: Option<f64> = None;
    let flush = |out: &mut Vec<Col>, vals: &mut Vec<f64>, c: i64, gap: bool| {
        if let Some(sm) = crate::stats::summarize(vals) {
            out.push(Col {
                x: tf.rect.x0 + (c as f32 + 0.5) * w,
                w,
                min: sm.min,
                max: sm.max,
                iqr: Some((sm.p25, sm.p75)),
                mid: sm.median,
                gap,
            });
        }
        vals.clear();
    };
    for (t, v) in items {
        let Some(v) = v else {
            if let Some(c) = cur.take() {
                flush(out, vals, c, gap);
            }
            gap = true;
            continue;
        };
        let c = col_of(t);
        if prev_t.is_some_and(|p| t - p > gap_limit) {
            if let Some(c) = cur.take() {
                flush(out, vals, c, gap);
            }
            gap = true;
        }
        if cur != Some(c) {
            if let Some(c) = cur {
                flush(out, vals, c, gap);
                gap = false;
            }
            cur = Some(c);
        }
        vals.push(v);
        prev_t = Some(t);
    }
    if let Some(c) = cur {
        flush(out, vals, c, gap);
    }
}

/// Columns from aggregate buckets (range and mean), for views too long to
/// walk raw samples. Empty buckets break the line.
fn columns_tier(out: &mut Vec<Col>, tier: &Tier, tf: &Transform, until: f64) {
    out.clear();
    let bw = (tier.width / (tf.t1 - tf.t0).max(1e-9)) as f32 * tf.rect.width();
    let w = palette::COLUMN.max(bw);
    let mut cur: Option<(i64, f64, f64, f64, u32, bool)> = None;
    let mut gap = false;
    let push = |out: &mut Vec<Col>, (c, min, max, sum, n, gap): (i64, f64, f64, f64, u32, bool)| {
        out.push(Col {
            x: tf.rect.x0 + (c as f32 + 0.5) * w,
            w,
            min,
            max,
            iqr: None,
            mid: sum / f64::from(n.max(1)),
            gap,
        });
    };
    let mut i = tier.first_ending_after(tf.t0);
    while let Some(bk) = tier.get(i) {
        if bk.t0 > tf.t1 || bk.t0 >= until {
            break;
        }
        i += 1;
        if bk.n == 0 {
            if let Some(c) = cur.take() {
                push(out, c);
            }
            gap = true;
            continue;
        }
        let c = ((tf.x(bk.t0 + tier.width / 2.0) - tf.rect.x0) / w).floor() as i64;
        match &mut cur {
            Some(a) if a.0 == c => {
                a.1 = a.1.min(bk.min);
                a.2 = a.2.max(bk.max);
                a.3 += bk.sum;
                a.4 += bk.n;
            }
            _ => {
                if let Some(a) = cur.take() {
                    push(out, a);
                }
                cur = Some((c, bk.min, bk.max, bk.sum, bk.n, gap));
                gap = false;
            }
        }
    }
    if let Some(a) = cur {
        push(out, a);
    }
}

/// Per column, a faint band over the full range and a stronger one over
/// the middle half, and a line through the medians: the typical value
/// reads first, every extreme is still drawn.
fn draw_columns(
    sc: &mut Scene,
    cols: &[Col],
    tf: &Transform,
    color: Color,
    width: f32,
    pts: &mut Vec<Point>,
) {
    let plot = tf.rect;
    for c in cols {
        let bands = [(c.min, c.max, palette::RANGE_ALPHA)]
            .into_iter()
            .chain(c.iqr.map(|(a, b)| (a, b, palette::IQR_ALPHA)));
        for (lo, hi, alpha) in bands {
            let (y0, y1) = (tf.yc(hi), tf.yc(lo));
            if y1 - y0 >= 0.5 {
                sc.fill(
                    Rect {
                        x0: (c.x - c.w / 2.0).max(plot.x0),
                        y0,
                        x1: (c.x + c.w / 2.0).min(plot.x1),
                        y1,
                    },
                    color,
                    alpha,
                );
            }
        }
    }
    pts.clear();
    for c in cols {
        if c.gap && !pts.is_empty() {
            sc.line(pts, width, color, 1.0);
            pts.clear();
        }
        pts.push(pt(c.x, tf.yc(c.mid)));
    }
    sc.line(pts, width, color, 1.0);
}

/// Whether samples this dense are drawn as columns.
fn banded(visible_samples: usize, plot: Rect) -> bool {
    visible_samples as f32 > plot.width() / palette::COLUMN * 1.5
}

/// Whether a view is so long that per-minute aggregates are no coarser
/// than a column, so they can stand in for the raw samples.
fn tier_columns(tf: &Transform) -> bool {
    let secs_per_pt = (tf.t1 - tf.t0) / f64::from(tf.rect.width().max(1.0));
    secs_per_pt * f64::from(palette::COLUMN) >= 60.0
}

fn bucket_at(tier: &Tier, t: f64) -> Option<&Bucket> {
    let i = tier.first_ending_after(t);
    tier.get(i).filter(|b| b.t0 <= t)
}

fn wall(inp: &Inputs<'_>, t: f64) -> String {
    format::clock(inp.wall0 + Duration::from_secs_f64(t.max(0.0)), true)
}

fn aggregate_title(tier: &Tier) -> String {
    format!(
        "{} aggregate",
        if tier.width >= 3600.0 {
            "Hour"
        } else {
            "Minute"
        }
    )
}

/// Tooltip for the download/upload band or pause at time `t`, if any.
fn band_tip(inp: &Inputs<'_>, t: f64) -> Option<Tip> {
    let s = inp.s;
    if let Some(&(p0, p1)) = s
        .pauses
        .iter()
        .find(|(p0, p1)| *p0 <= t && p1.unwrap_or(inp.run_end) >= t)
    {
        let mut tip = Tip::default().title("Paused");
        tip.push_pair("From", wall(inp, p0));
        tip.push_pair("To", p1.map_or("now".to_owned(), |p| wall(inp, p)));
        return Some(tip);
    }
    let x = s
        .transfers(t, t, 0)
        .find(|x| x.t_start() <= t && x.t_end() >= t)?;
    let tip = transfer_tip(inp, &x, false);
    Some(tip.note("Shaded while a download or upload is running."))
}

impl Charts {
    fn throughput_stats(&mut self, s: &Session, view: &TimeView, t0: f64, t1: f64) -> TputStats {
        let vals = &mut self.vals;
        self.tput_stats.get(stats_key(s, view), || {
            let mut bins_max: f64 = 0.0;
            vals.clear();
            for series in &s.throughput {
                let (a, b) = visible(series, t0, t1);
                for x in series.raw.range(a, b) {
                    bins_max = bins_max.max(x.bps);
                    vals.push(x.bps);
                }
                if let Some((tier, until)) = aged_tier(series, t0, t1) {
                    for bk in tier
                        .iter()
                        .filter(|bk| bk.t0 + tier.width > t0 && bk.t0 < until && bk.n > 0)
                    {
                        bins_max = bins_max.max(bk.max);
                    }
                }
            }
            let n_bins = vals.len();
            let bins_p99 = crate::stats::summarize(vals).map(|sm| sm.p99);
            let (mut req_max, mut dur, mut n) = (0.0f64, 0.0, 0u32);
            for x in s.transfers(t0, t1, 0) {
                req_max = req_max.max(x.bps().unwrap_or(0.0));
                dur += x.t_end() - x.t_start();
                n += 1;
            }
            let mut pct = [None, None];
            for (i, dir) in [Direction::Download, Direction::Upload]
                .into_iter()
                .enumerate()
            {
                vals.clear();
                vals.extend(
                    s.transfers(t0, t1, 0)
                        .filter(|x| x.dir() == dir)
                        .filter_map(|x| x.bps()),
                );
                pct[i] = crate::stats::summarize(vals).map(|sm| (sm.median, sm.p90));
            }
            TputStats {
                bins_top: robust_top(bins_max, bins_p99, n_bins),
                bins_max,
                req_max,
                mean_req_secs: if n > 0 { dur / f64::from(n) } else { 0.0 },
                pct,
            }
        })
    }

    /// The transform, whether there is anything to draw, the per-request
    /// percentiles, and whether requests are drawn as points (too narrow
    /// at this zoom to show their bins).
    fn throughput_tf(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
    ) -> (Transform, bool, [Pct; 2], bool, Option<f64>) {
        let (t0, t1) = view.range(inp.run_end);
        let st = self.throughput_stats(inp.s, view, t0, t1);
        let rect = time_plot(w, h);
        let secs_per_pt = (t1 - t0) / f64::from(rect.width().max(1.0));
        let narrow = st.req_max > 0.0
            && st.mean_req_secs < f64::from(palette::REQUEST_MIN_WIDTH) * secs_per_pt;
        let (vmax, top) = if narrow {
            (st.req_max, nice_max(st.req_max))
        } else {
            let max = st.bins_max.max(st.req_max);
            (max, st.bins_top.max(nice_max(st.req_max)))
        };
        let tf = Transform {
            rect,
            t0,
            t1,
            v0: 0.0,
            v1: top,
        };
        // The maximum, when rare spikes run past the top of the axis.
        let cut = (vmax > top).then_some(vmax);
        (tf, vmax > 0.0, st.pct, narrow, cut)
    }

    /// Throughput chart of `w`×`h` points.
    pub fn throughput(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
        label_h: f32,
        sc: &mut Scene,
    ) {
        let (tf, has_data, pct, narrow, cut) = self.throughput_tf(inp, view, w, h);
        let plot = tf.rect;
        let s = inp.s;
        sc.clear(plot);
        if has_data {
            nice_ticks(
                0.0,
                tf.v1,
                (plot.height() / palette::TICK_SPACING_Y) as usize,
                &mut self.ticks,
            );
            value_axis(sc, &tf, &self.ticks, &format::si, "bps");
        }
        let base = plot.y1.round() + 0.5;
        sc.segment(
            pt(plot.x0, base),
            pt(plot.x1, base),
            palette::STROKE,
            Color::Grid,
            1.0,
        );
        if s.wall_start.is_some() {
            time_axis(sc, &tf, inp.wall0, &mut self.ticks);
        }
        let (t0, t1) = (tf.t0, tf.t1);
        sc.clip_x(Some((plot.x0, plot.x1)));
        for (di, dir) in [Direction::Download, Direction::Upload]
            .into_iter()
            .enumerate()
        {
            let series = &s.throughput[di];
            let color = dir_color(dir);
            let (a, e) = visible(series, t0, t1);
            let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
            if narrow {
                // One point per transfer at its throughput, joined in time
                // order: the trend across cycles.
                self.aux.clear();
                transfer_trend(&mut dec, &mut self.aux, &mut self.vals, s, dir, &tf);
            } else if coarse(e - a, plot) {
                feed_buckets(&mut dec, tier_for(series, t0), &tf);
            } else {
                if let Some((tier, until)) = aged_tier(series, t0, t1) {
                    draw_buckets(sc, tier, until, &tf, color, &mut self.aux);
                }
                let mut prev: Option<(u32, f64)> = None;
                let mut skip: Option<(u32, bool)> = None;
                for x in series.raw.range(a, e) {
                    // Transfers too narrow to show their bins are drawn as
                    // one point below instead.
                    let narrow_req = match skip {
                        Some((id, n)) if id == x.request => n,
                        _ => {
                            let n = s.transfer(x.request).is_some_and(|r| too_narrow(&tf, &r));
                            skip = Some((x.request, n));
                            n
                        }
                    };
                    if narrow_req {
                        continue;
                    }
                    if let Some((req, pt)) = prev
                        && (req != x.request || x.t - pt > 0.5)
                    {
                        dec.gap();
                    }
                    dec.push(tf.posc(x.t, x.bps));
                    prev = Some((x.request, x.t));
                }
                self.aux.clear();
                self.aux.extend(
                    s.transfers(t0, t1, 0)
                        .filter(|x| x.dir() == dir && too_narrow(&tf, x))
                        .filter_map(|x| Some(tf.pos(mid(&x), x.bps()?))),
                );
            }
            dec.finish();
            if !narrow {
                for (i, j) in segments(&self.breaks) {
                    sc.area(&self.pts[i..j], plot.y1, color, palette::FILL_ALPHA);
                }
            }
            for (i, j) in segments(&self.breaks) {
                sc.line(&self.pts[i..j], palette::DATA_LINE, color, 1.0);
            }
            if !narrow || self.aux.len() as f32 <= plot.width() / palette::DOT_MIN_SPACING {
                sc.dots(&self.aux, palette::POINT_RADIUS, color, 1.0);
            }
            // Percentiles of transfer throughput in view.
            if let Some((p50, p90)) = pct[di] {
                for v in [p50, p90] {
                    let y = tf.y(v).round() + 0.5;
                    sc.dashed_hline(plot.x0, plot.x1, y, color, palette::PERCENTILE_ALPHA);
                }
            }
        }
        sc.clip_x(None);
        let mut labels = Vec::new();
        for (di, dir) in [Direction::Download, Direction::Upload]
            .into_iter()
            .enumerate()
        {
            if let Some((p50, p90)) = pct[di] {
                for (name, v) in [("p50", p50), ("p90", p90)] {
                    labels.push((tf.y(v), format!("{name} {}", format::si(v)), dir_color(dir)));
                }
            }
        }
        if let Some(max) = cut {
            labels.push((plot.y0, format!("max {}", format::si(max)), Color::Text2));
        }
        percentile_labels(sc, plot, label_h, &mut labels);
    }

    pub fn throughput_hover(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
        hp: Point,
    ) -> Hover {
        let (tf, has_data, _, narrow, _) = self.throughput_tf(inp, view, w, h);
        let plot = tf.rect;
        let s = inp.s;
        if !has_data || !plot.contains(hp) {
            return Hover::default();
        }
        if narrow {
            return transfer_hover(inp, &tf, hp, |_| true).map_or_else(Hover::default, |h| h.0);
        }
        // Transfers too narrow for their bins are drawn as points.
        let req = transfer_hover(inp, &tf, hp, |x| too_narrow(&tf, x));
        let th = tf.t_at(hp.x);
        if let Some((tier, until)) =
            aged_tier(&s.throughput[0], tf.t0, tf.t1).or(aged_tier(&s.throughput[1], tf.t0, tf.t1))
            && th < until
            && let Some(bk) = bucket_at(tier, th)
        {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            let tip = Tip::default()
                .title(aggregate_title(tier))
                .pair("Start", wall(inp, bk.t0))
                .pair("Bins", format::count(u64::from(bk.n)))
                .pair("Min", format!("{} Mbps", format::mbps(bk.min)))
                .pair("Mean", format!("{} Mbps", format::mbps(bk.mean().unwrap_or(0.0))))
                .pair("Max", format!("{} Mbps", format::mbps(bk.max)))
                .note("Raw bins older than the sample buffer are kept as per-minute and per-hour aggregates.");
            return Hover {
                crosshair: Some(x),
                highlight: None,
                tip: Some(tip),
                anchor: pt(x, hp.y),
            };
        }
        // Nearest bin in either direction, by horizontal distance, then
        // vertical when both are close.
        // Bins of narrow transfers aren't drawn, so they're skipped.
        let shown = |x: &crate::model::ThroughputSample| {
            !s.transfer(x.request).is_some_and(|r| too_narrow(&tf, &r))
        };
        let mut best: Option<(f32, usize, usize)> = None;
        for (di, series) in s.throughput.iter().enumerate() {
            let i = series.raw.partition_point(|x| x.t < th);
            let left = (i.saturating_sub(BIN_SEARCH)..i)
                .rev()
                .find(|&j| series.raw.get(j).is_some_and(shown));
            let right = (i..(i + BIN_SEARCH).min(series.raw.len()))
                .find(|&j| series.raw.get(j).is_some_and(shown));
            for j in [left, right].into_iter().flatten() {
                if let Some(x) = series.raw.get(j) {
                    let p = tf.posc(x.t, x.bps);
                    let d = (p.x - hp.x).abs() + 0.25 * (p.y - hp.y).abs();
                    if best.is_none_or(|(bd, _, _)| d < bd) {
                        best = Some((d, di, j));
                    }
                }
            }
        }
        if let Some((hv, d)) = req
            && best.is_none_or(|(bd, _, _)| d <= bd)
        {
            return hv;
        }
        let Some((_, di, j)) = best else {
            return Hover::default();
        };
        let Some(x) = s.throughput[di].raw.get(j).copied() else {
            return Hover::default();
        };
        let p = tf.posc(x.t, x.bps);
        if (p.x - hp.x).abs() > palette::SNAP_PX {
            return Hover::default();
        }
        let xfer = s.transfer(x.request);
        let mut tip = match xfer {
            Some(Transfer::Sustained(r)) => {
                Tip::default().title(format!("{} · {} connections", r.dir.label(), r.streams))
            }
            Some(Transfer::Single(r)) => Tip::default().title(format!(
                "{} {} · sample {}/{}",
                size_label(r.size),
                x.dir.lower(),
                r.index,
                r.count
            )),
            None => Tip::default().title(format!("{} · in progress", x.dir.label())),
        };
        tip.push_pair("Throughput", format!("{} Mbps", format::mbps(x.bps)));
        tip.push_pair("Time", wall(inp, x.t));
        tip.push_pair(
            "Bin",
            format!("{} · {:.0} ms", x.bin + 1, f64::from(x.dt) * 1000.0),
        );
        if let Some(b) = xfer.and_then(|r| r.bps()) {
            tip.push_pair("Whole transfer", format!("{} Mbps", format::mbps(b)));
        }
        let tip = tip.note(match x.dir {
            Direction::Download => {
                "Body bytes received on every connection in the bin, divided by the bin length."
            }
            Direction::Upload => {
                "Bytes the server acknowledged on every connection in the bin (kernel TCP counters), divided by the bin length."
            }
        });
        Hover {
            crosshair: Some(p.x),
            highlight: Some((p, dir_color(x.dir))),
            tip: Some(tip),
            anchor: p,
        }
    }

    fn latency_stats(&mut self, s: &Session, view: &TimeView, t0: f64, t1: f64) -> LatStats {
        let (lat, gw) = (&s.latency, &s.gateway);
        let (la, lb) = visible(lat, t0, t1);
        let (ga, gb) = visible(gw, t0, t1);
        let vals = &mut self.vals;
        self.lat_stats.get(stats_key(s, view), || {
            let mut max: f64 = 0.0;
            if let Some((tier, until)) = aged_tier(lat, t0, t1) {
                for bk in tier
                    .iter()
                    .filter(|bk| bk.t0 + tier.width > t0 && bk.t0 < until && bk.n > 0)
                {
                    max = max.max(bk.max);
                }
            }
            let gw_sm = crate::stats::summarize_iter(
                vals,
                gw.raw
                    .range(ga, gb)
                    .filter(|x| x.t >= t0 && x.t <= t1)
                    .filter_map(|x| x.ms),
            );
            let lat_sm = crate::stats::summarize_iter(
                vals,
                lat.raw
                    .range(la, lb)
                    .filter(|x| x.t >= t0 && x.t <= t1)
                    .map(|x| x.ms),
            );
            let mut n = 0;
            let mut p99: Option<f64> = None;
            for sm in [lat_sm, gw_sm].into_iter().flatten() {
                max = max.max(sm.max);
                n += sm.n;
                p99 = Some(p99.unwrap_or(0.0).max(sm.p99));
            }
            LatStats {
                max,
                top: robust_top(max, p99, n),
                pct: lat_sm.map(|sm| (sm.median, sm.p90)),
            }
        })
    }

    /// The transform (its rect stops above the loss lane), whether there
    /// is anything to draw, and the statistics.
    fn latency_tf(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
    ) -> (Transform, bool, LatStats) {
        let (t0, t1) = view.range(inp.run_end);
        let st = self.latency_stats(inp.s, view, t0, t1);
        let mut rect = time_plot(w, h);
        rect.y1 = (rect.y1 - palette::LOSS_LANE).max(rect.y0 + 1.0);
        let tf = Transform {
            rect,
            t0,
            t1,
            v0: 0.0,
            v1: st.top,
        };
        (tf, st.max > 0.0, st)
    }

    /// Latency over time with gateway pings, load periods and loss ticks.
    pub fn latency(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
        label_h: f32,
        sc: &mut Scene,
    ) {
        let (tf, has_data, st) = self.latency_tf(inp, view, w, h);
        let plot = time_plot(w, h);
        let data = tf.rect;
        let (t0, t1) = (tf.t0, tf.t1);
        let s = inp.s;
        let (lat, gw) = (&s.latency, &s.gateway);
        let (la, lb) = visible(lat, t0, t1);
        let (ga, gb) = visible(gw, t0, t1);
        sc.clear(plot);
        if has_data {
            nice_ticks(
                0.0,
                tf.v1,
                (data.height() / palette::TICK_SPACING_Y) as usize,
                &mut self.ticks,
            );
            value_axis(sc, &tf, &self.ticks, &format::tick, "ms");
        }
        let base = data.y1.round() + 0.5;
        sc.segment(
            pt(plot.x0, base),
            pt(plot.x1, base),
            palette::STROKE,
            Color::Grid,
            1.0,
        );
        if s.wall_start.is_some() {
            time_axis(
                sc,
                &Transform { rect: plot, ..tf },
                inp.wall0,
                &mut self.ticks,
            );
        }
        let gap_limit = (3.5 * s.config.continuous.latency_interval.as_secs_f64()).max(2.0);
        // Load periods: when a download or upload was running.
        // The one running now has no record yet.
        let running = match s.phase {
            Some(Phase::Sustained { dir, started, .. }) if s.state == RunState::Running => {
                Some((dir, started, inp.run_end))
            }
            _ => None,
        };
        let bands = s
            .transfers(t0, t1, 0)
            .map(|x| (x.dir(), x.t_start(), x.t_end()))
            .chain(running);
        for (dir, a, b) in bands {
            let x0 = tf.x(a).max(plot.x0);
            let x1 = tf.x(b).min(plot.x1);
            if x1 > x0 {
                sc.fill(
                    Rect {
                        x0,
                        y0: plot.y0,
                        x1,
                        y1: data.y1,
                    },
                    dir_color(dir),
                    palette::BAND_ALPHA,
                );
            }
        }
        // Paused periods: nothing was measured.
        for &(p0, p1) in s.pauses.iter() {
            let p1 = p1.unwrap_or(inp.run_end);
            let x0 = tf.x(p0).max(plot.x0);
            let x1 = tf.x(p1).min(plot.x1);
            if x1 > x0 {
                sc.fill(
                    Rect {
                        x0,
                        y0: plot.y0,
                        x1,
                        y1: data.y1,
                    },
                    Color::Text3,
                    palette::BAND_ALPHA,
                );
            }
        }
        sc.clip_x(Some((plot.x0, plot.x1)));
        let by_tier = tier_columns(&tf);
        // Gateway pings, then latency over them.
        if banded(gb - ga, data) {
            if by_tier {
                columns_tier(&mut self.cols, tier_for(gw, t0), &tf, f64::INFINITY);
            } else {
                columns_raw(
                    &mut self.cols,
                    &mut self.vals,
                    &tf,
                    gap_limit,
                    gw.raw.range(ga, gb).map(|x| (x.t, x.ms)),
                );
            }
            draw_columns(sc, &self.cols, &tf, GATEWAY, palette::STROKE, &mut self.pts);
        } else {
            let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
            let mut prev_t: Option<f64> = None;
            for x in gw.raw.range(ga, gb) {
                match x.ms {
                    Some(m) => {
                        if prev_t.is_some_and(|p| x.t - p > gap_limit) {
                            dec.gap();
                        }
                        dec.push(tf.posc(x.t, m));
                        prev_t = Some(x.t);
                    }
                    None => dec.gap(),
                }
            }
            dec.finish();
            for (i, j) in segments(&self.breaks) {
                sc.line(&self.pts[i..j], palette::STROKE, GATEWAY, 1.0);
            }
        }
        if !by_tier && let Some((tier, until)) = aged_tier(lat, t0, t1) {
            draw_buckets(sc, tier, until, &tf, Color::Latency, &mut self.aux);
        }
        if banded(lb - la, data) {
            if by_tier {
                columns_tier(&mut self.cols, tier_for(lat, t0), &tf, f64::INFINITY);
            } else {
                columns_raw(
                    &mut self.cols,
                    &mut self.vals,
                    &tf,
                    gap_limit,
                    lat.raw.range(la, lb).map(|x| (x.t, Some(x.ms))),
                );
            }
            draw_columns(
                sc,
                &self.cols,
                &tf,
                Color::Latency,
                palette::DATA_LINE,
                &mut self.pts,
            );
        } else {
            let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
            let mut prev_t: Option<f64> = None;
            for x in lat.raw.range(la, lb) {
                if prev_t.is_some_and(|p| x.t - p > gap_limit) {
                    dec.gap();
                }
                dec.push(tf.posc(x.t, x.ms));
                prev_t = Some(x.t);
            }
            dec.finish();
            for (i, j) in segments(&self.breaks) {
                sc.line(&self.pts[i..j], palette::DATA_LINE, Color::Latency, 1.0);
            }
            if (lb - la) as f32 <= data.width() / palette::DOT_MIN_SPACING {
                self.aux.clear();
                self.aux
                    .extend(lat.raw.range(la, lb).map(|x| tf.posc(x.t, x.ms)));
                sc.dots(&self.aux, palette::POINT_RADIUS, Color::Latency, 1.0);
            }
        }
        sc.clip_x(None);
        // Lost UDP probes, in their own lane under the plot.
        loss_ticks(
            sc,
            s,
            &tf,
            plot.y1,
            plot.y1 - palette::LOSS_TICK,
            palette::DATA_LINE,
        );
        // Percentiles of visible latency, and the maximum when spikes run
        // past the top of the axis.
        let mut labels: Vec<(f32, String, Color)> = Vec::new();
        if let Some((p50, p90)) = st.pct {
            for v in [p50, p90] {
                sc.dashed_hline(
                    plot.x0,
                    plot.x1,
                    tf.y(v).round() + 0.5,
                    Color::Latency,
                    palette::PERCENTILE_ALPHA,
                );
            }
            labels.extend([("p50", p50), ("p90", p90)].iter().map(|(name, v)| {
                (
                    tf.y(*v),
                    format!("{name} {} ms", format::ms(*v)),
                    Color::Latency,
                )
            }));
        }
        if st.max > tf.v1 {
            labels.push((
                data.y0,
                format!("max {} ms", format::ms(st.max)),
                Color::Text2,
            ));
        }
        percentile_labels(sc, data, label_h, &mut labels);
    }

    /// Summary of the column under the pointer when latency is drawn as
    /// columns: every probe in it, the gateway pings and lost UDP probes.
    fn column_hover(&mut self, inp: &Inputs<'_>, tf: &Transform, hp: Point) -> Option<Hover> {
        let s = inp.s;
        let w = palette::COLUMN;
        let c = ((hp.x - tf.rect.x0) / w).floor();
        let (x0, x1) = (tf.rect.x0 + c * w, tf.rect.x0 + (c + 1.0) * w);
        let (ta, tb) = (tf.t_at(x0), tf.t_at(x1));
        let (la, lb) = s.latency.raw_range(ta, tb);
        let lat = crate::stats::summarize_iter(
            &mut self.vals,
            s.latency.raw.range(la, lb).map(|x| x.ms),
        )?;
        let (ga, gb) = s.gateway.raw_range(ta, tb);
        let gw = crate::stats::summarize_iter(
            &mut self.vals,
            s.gateway.raw.range(ga, gb).filter_map(|x| x.ms),
        );
        let (pa, pb) = s.loss.raw_range(ta, tb);
        let lost = s
            .loss
            .raw
            .range(pa, pb)
            .filter(|p| p.outcome == ProbeOutcome::Lost)
            .count();
        let mut tip = Tip::default()
            .title(format!("Latency · {} – {}", wall(inp, ta), wall(inp, tb)))
            .pair("Probes", format::count(lat.n as u64))
            .pair("Min", format!("{} ms", format::ms(lat.min)))
            .pair("Median", format!("{} ms", format::ms(lat.median)))
            .pair("Max", format!("{} ms", format::ms(lat.max)));
        if let Some(g) = gw {
            tip.push_pair("Gateway median", format!("{} ms", format::ms(g.median)));
        }
        tip.push_pair(
            "UDP probes lost",
            format!(
                "{} of {}",
                format::count(lost as u64),
                format::count((pb - pa) as u64)
            ),
        );
        let tip = tip.note(
            "Each column spans the probes in this time range: the band is their range, the line their median. Zoom in to see every probe.",
        );
        let x = (x0 + x1) / 2.0;
        let p = pt(x, tf.yc(lat.median));
        Some(Hover {
            crosshair: Some(x),
            highlight: Some((p, Color::Latency)),
            tip: Some(tip),
            anchor: p,
        })
    }

    pub fn latency_hover(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
        hp: Point,
    ) -> Hover {
        let (tf, has_data, _) = self.latency_tf(inp, view, w, h);
        let plot = time_plot(w, h);
        let s = inp.s;
        if !plot.contains(hp) {
            return Hover::default();
        }
        let th = tf.t_at(hp.x);
        let (lat, gw) = (&s.latency, &s.gateway);

        // Loss ticks take the hover near the bottom edge.
        if hp.y > tf.rect.y1 {
            let i = s.loss.raw.partition_point(|p| p.t < th);
            let near = s
                .loss
                .raw
                .range(i.saturating_sub(64), (i + 64).min(s.loss.raw.len()))
                .filter(|p| p.outcome == ProbeOutcome::Lost)
                .min_by(|a, b| {
                    (tf.x(a.t) - hp.x)
                        .abs()
                        .total_cmp(&(tf.x(b.t) - hp.x).abs())
                });
            if let Some(p) = near {
                let x = tf.x(p.t);
                if (x - hp.x).abs() <= palette::HOVER_PX {
                    let tip = Tip::default()
                        .title(format!("UDP probe {} lost · {}", p.seq, p.load.label().to_lowercase()))
                        .pair("Sent", wall(inp, p.t))
                        .note("STUN Binding request to stun.cloudflare.com with no answer within 3 s.");
                    return Hover {
                        crosshair: Some(x),
                        highlight: None,
                        tip: Some(tip),
                        anchor: pt(x, plot.y1 - palette::LOSS_TICK),
                    };
                }
            }
        }
        if !has_data {
            return Hover::default();
        }
        if let Some((tier, until)) = aged_tier(lat, tf.t0, tf.t1)
            && th < until
            && let Some(bk) = bucket_at(tier, th)
        {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            let mut tip = Tip::default()
                .title(aggregate_title(tier))
                .pair("Start", wall(inp, bk.t0))
                .pair("Probes", format::count(u64::from(bk.n)))
                .pair("Min", format!("{} ms", format::ms(bk.min)))
                .pair(
                    "Mean",
                    format!("{} ms", format::ms(bk.mean().unwrap_or(0.0))),
                )
                .pair("Max", format!("{} ms", format::ms(bk.max)));
            if let Some(j) = bk.jitter() {
                tip.push_pair("Jitter", format!("{} ms", format::ms(j)));
            }
            return Hover {
                crosshair: Some(x),
                highlight: None,
                tip: Some(tip),
                anchor: pt(x, hp.y),
            };
        }
        let (la, lb) = visible(lat, tf.t0, tf.t1);
        if banded(lb - la, tf.rect)
            && let Some(hv) = self.column_hover(inp, &tf, hp)
        {
            return hv;
        }
        let i = lat.raw.partition_point(|x| x.t < th);
        let best_lat = [i.saturating_sub(1), i]
            .into_iter()
            .filter_map(|j| lat.raw.get(j).copied())
            .min_by(|a, b| {
                (tf.x(a.t) - hp.x)
                    .abs()
                    .total_cmp(&(tf.x(b.t) - hp.x).abs())
            });
        let gi = gw.raw.partition_point(|x| x.t < th);
        let best_gw = [gi.saturating_sub(1), gi]
            .into_iter()
            .filter_map(|j| gw.raw.get(j).copied())
            .filter(|g| g.ms.is_some())
            .min_by(|a, b| {
                (tf.x(a.t) - hp.x)
                    .abs()
                    .total_cmp(&(tf.x(b.t) - hp.x).abs())
            });
        let lat_p = best_lat.map(|x| tf.posc(x.t, x.ms));
        let gw_p = best_gw.and_then(|g| g.ms.map(|m| tf.posc(g.t, m)));
        let score = |p: Point| (p.x - hp.x).abs() + (p.y - hp.y).abs() * 0.5;
        let use_gw = match (lat_p, gw_p) {
            (Some(a), Some(g)) => score(g) < score(a),
            (None, Some(_)) => true,
            _ => false,
        };
        let near = |p: Option<Point>| p.is_some_and(|p| (p.x - hp.x).abs() <= palette::SNAP_PX);
        if !near(lat_p) && !near(gw_p) {
            // No probe nearby: describe the load band under the pointer.
            return Hover {
                tip: band_tip(inp, th),
                anchor: hp,
                ..Hover::default()
            };
        }
        if use_gw && let (Some(g), Some(p)) = (best_gw, gw_p) {
            if (p.x - hp.x).abs() > palette::SNAP_PX {
                return Hover::default();
            }
            let gwip = s
                .iface
                .as_ref()
                .and_then(|i| i.gateway)
                .map(|g| g.to_string())
                .unwrap_or_default();
            let tip = Tip::default()
                .title(format!(
                    "Gateway {gwip} · {}",
                    g.load.label().to_lowercase()
                ))
                .pair(
                    "Round trip",
                    format!("{} ms", format::ms(g.ms.unwrap_or(0.0))),
                )
                .pair("Time", wall(inp, g.t))
                .note("ICMP echo to the default gateway.");
            return Hover {
                crosshair: Some(p.x),
                highlight: Some((p, GATEWAY)),
                tip: Some(tip),
                anchor: p,
            };
        }
        let (Some(x), Some(p)) = (best_lat, lat_p) else {
            return Hover::default();
        };
        if (p.x - hp.x).abs() > palette::SNAP_PX {
            return Hover::default();
        }
        let mut tip = Tip::default().title(format!("Latency · {}", x.load.label().to_lowercase()));
        tip.push_pair("Latency", format!("{} ms", format::ms(x.ms)));
        tip.push_pair("TTFB", format!("{} ms", format::ms(x.ttfb_ms)));
        if let Some(sv) = x.server_ms {
            tip.push_pair("Server time", format!("{} ms", format::ms(sv)));
        }
        if let Some(r) = x.server_rtt_ms {
            tip.push_pair("Edge TCP RTT", format!("{} ms", format::ms(r)));
        }
        tip.push_pair("Time", wall(inp, x.t));
        if x.count > 0 {
            tip.push_pair("Probe", format!("{}/{}", x.index, x.count));
        }
        tip.push_pair("Connection", format!("#{}", x.conn));
        let tip = tip.note(
            "Time to first byte of GET __down?bytes=0 on a warm connection, minus the server time in Server-Timing.",
        );
        Hover {
            crosshair: Some(p.x),
            highlight: Some((p, Color::Latency)),
            tip: Some(tip),
            anchor: p,
        }
    }

    /// Whole-run strip with the visible window marked.
    pub fn overview(&mut self, inp: &Inputs<'_>, view: &TimeView, w: f32, h: f32, sc: &mut Scene) {
        let (tf, narrow) = overview_tf(inp, w, h);
        let plot = tf.rect;
        let s = inp.s;
        sc.clear(plot);
        sc.outline(plot, Color::Grid, 1.0);
        for (di, series) in s.throughput.iter().enumerate() {
            let color = if di == 0 {
                Color::Download
            } else {
                Color::Upload
            };
            // Raw bins while every request is wide enough to show them,
            // otherwise the per-cycle trend.
            let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
            if narrow {
                self.aux.clear();
                transfer_trend(&mut dec, &mut self.aux, &mut self.vals, s, dir_of(di), &tf);
                dec.finish();
                for (i, j) in segments(&self.breaks) {
                    sc.line(&self.pts[i..j], palette::STROKE, color, 1.0);
                }
                continue;
            }
            if series.raw_complete() && !coarse(series.raw.len(), plot) {
                let mut prev: Option<u32> = None;
                for x in series.raw.iter() {
                    if prev.is_some_and(|r| r != x.request) {
                        dec.gap();
                    }
                    dec.push(tf.pos(x.t, x.bps));
                    prev = Some(x.request);
                }
            } else {
                let half = series.minute.width / 2.0;
                for bk in series.minute.iter() {
                    match bk.mean() {
                        Some(_) => dec.push(tf.pos(bk.t0 + half, bk.max)),
                        None => dec.gap(),
                    }
                }
            }
            dec.finish();
            for (i, j) in segments(&self.breaks) {
                sc.area(
                    &self.pts[i..j],
                    plot.y1 - 1.0,
                    color,
                    palette::OVERVIEW_FILL_ALPHA,
                );
            }
        }
        loss_ticks(
            sc,
            s,
            &tf,
            plot.y1 - 1.0,
            plot.y1 - palette::S8,
            palette::STROKE,
        );
        // Visible window.
        let (t0, t1) = view.range(tf.t1);
        if view.span.is_some() {
            let (x0, x1) = (tf.x(t0), tf.x(t1).max(tf.x(t0) + 2.0));
            let band = |a: f32, b: f32| Rect {
                x0: a,
                y0: plot.y0,
                x1: b,
                y1: plot.y1,
            };
            sc.fill(
                band(plot.x0, x0),
                Color::Background,
                palette::OUTSIDE_VIEW_ALPHA,
            );
            sc.fill(
                band(x1, plot.x1),
                Color::Background,
                palette::OUTSIDE_VIEW_ALPHA,
            );
            sc.outline(band(x0, x1), Color::Accent, 1.0);
        }
        if s.wall_start.is_some() {
            sc.text(
                pt(plot.x1 + palette::S8, plot.center_y()),
                &format::elapsed(tf.t1),
                TextSize::Small,
                Color::Text2,
                HAlign::Start,
                VAlign::Center,
            );
        }
    }

    pub fn overview_hover(inp: &Inputs<'_>, w: f32, h: f32, hp: Point) -> Hover {
        let (tf, _) = overview_tf(inp, w, h);
        if inp.s.wall_start.is_none() || !tf.rect.contains(hp) {
            return Hover::default();
        }
        let t = tf.t_at(hp.x);
        let tip = Tip::default()
            .title("Whole run")
            .pair("Time", wall(inp, t))
            .pair("Elapsed", format::elapsed(t));
        Hover {
            tip: Some(tip),
            anchor: hp,
            ..Hover::default()
        }
    }

    /// Whether the latency chart's scene changes with the clock alone (an
    /// ongoing pause band grows without new data).
    pub fn latency_ticks_with_clock(s: &Session) -> bool {
        s.state == RunState::Paused
    }
}

/// The overview strip spans the whole run across the time plots' extent.
/// True with it when transfers are too narrow at this scale to show their
/// bins, so it draws the per-cycle trend.
pub fn overview_tf(inp: &Inputs<'_>, w: f32, h: f32) -> (Transform, bool) {
    let x = time_plot(w, h);
    let rect = Rect {
        x0: x.x0,
        y0: 0.0,
        x1: x.x1,
        y1: h,
    };
    let t1 = inp.run_end.max(1.0);
    let s = inp.s;
    let (mut req_max, mut dur, mut n) = (0.0f64, 0.0, 0u32);
    for x in s.transfers(0.0, t1, 0) {
        req_max = req_max.max(x.bps().unwrap_or(0.0));
        dur += x.t_end() - x.t_start();
        n += 1;
    }
    let secs_per_pt = t1 / f64::from(rect.width().max(1.0));
    let narrow = n > 0 && dur / f64::from(n) < f64::from(palette::REQUEST_MIN_WIDTH) * secs_per_pt;
    let mut vmax: f64 = 0.0;
    if narrow {
        vmax = req_max;
    } else {
        for series in &s.throughput {
            for b in series.minute.iter() {
                if b.n > 0 {
                    vmax = vmax.max(b.max);
                }
            }
        }
    }
    let tf = Transform {
        rect,
        t0: 0.0,
        t1,
        v0: 0.0,
        v1: nice_max(vmax),
    };
    (tf, narrow)
}
