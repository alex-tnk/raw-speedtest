//! Throughput and latency over time, and the whole-run overview strip.

use std::time::{Duration, SystemTime};

use super::{
    Decimator, Hover, TimeView, Transform, nice_max, nice_ticks, percentile_labels, segments,
    time_axis, time_plot, value_axis,
};
use crate::app::session::{RunState, Session};
use crate::engine::size_label;
use crate::model::{Direction, ProbeOutcome};
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
    /// Axis maximum and per-direction (p50, p90) for the view.
    tput_stats: Memo<(f64, [Pct; 2])>,
    /// Axis maximum and (p50, p90) of latency for the view.
    lat_stats: Memo<(f64, Pct)>,
    pts: Vec<Point>,
    /// Second point buffer for aggregates drawn while `pts` is in use.
    aux: Vec<Point>,
    breaks: Vec<usize>,
    ticks: Vec<f64>,
    vals: Vec<f64>,
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
                pt(x, tf.y(bk.min)),
                pt(x, tf.y(bk.max)),
                palette::STROKE,
                color,
                palette::ENVELOPE_ALPHA,
            );
            pts.push(pt(x, tf.y(mean)));
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
    let i = s.requests.partition_point(|r| r.t_end < t);
    let r = s.requests.get(i).filter(|r| r.t_start <= t)?;
    let mut tip = Tip::default().title(format!(
        "{} {} · sample {}/{}",
        size_label(r.size),
        r.dir.lower(),
        r.index,
        r.count
    ));
    tip.push_pair("Start", wall(inp, r.t_start));
    tip.push_pair("End", wall(inp, r.t_end));
    if let Some(b) = r.bps {
        tip.push_pair("Throughput", format!("{} Mbps", format::mbps(b)));
    }
    Some(tip.note("Shaded while a download or upload request is in flight."))
}

impl Charts {
    fn throughput_stats(
        &mut self,
        s: &Session,
        view: &TimeView,
        t0: f64,
        t1: f64,
    ) -> (f64, [Pct; 2]) {
        let vals = &mut self.vals;
        self.tput_stats.get(stats_key(s, view), || {
            let mut vmax: f64 = 0.0;
            for series in &s.throughput {
                let (a, b) = visible(series, t0, t1);
                for x in series.raw.range(a, b) {
                    vmax = vmax.max(x.bps);
                }
                if let Some((tier, until)) = aged_tier(series, t0, t1) {
                    for bk in tier
                        .iter()
                        .filter(|bk| bk.t0 + tier.width > t0 && bk.t0 < until && bk.n > 0)
                    {
                        vmax = vmax.max(bk.max);
                    }
                }
            }
            let mut pct = [None, None];
            for (i, dir) in [Direction::Download, Direction::Upload]
                .into_iter()
                .enumerate()
            {
                vals.clear();
                vals.extend(
                    s.requests_in(dir)
                        .filter(|r| r.t_end >= t0 && r.t_start <= t1)
                        .filter_map(|r| r.bps),
                );
                pct[i] = crate::stats::summarize(vals).map(|sm| (sm.median, sm.p90));
            }
            (vmax, pct)
        })
    }

    fn throughput_tf(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
    ) -> (Transform, bool, [Pct; 2]) {
        let (t0, t1) = view.range(inp.run_end);
        let (vmax, pct) = self.throughput_stats(inp.s, view, t0, t1);
        let tf = Transform {
            rect: time_plot(w, h),
            t0,
            t1,
            v0: 0.0,
            v1: nice_max(vmax),
        };
        (tf, vmax > 0.0, pct)
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
        let (tf, has_data, pct) = self.throughput_tf(inp, view, w, h);
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
            if coarse(e - a, plot) {
                feed_buckets(&mut dec, tier_for(series, t0), &tf);
            } else {
                if let Some((tier, until)) = aged_tier(series, t0, t1) {
                    draw_buckets(sc, tier, until, &tf, color, &mut self.aux);
                }
                let mut prev: Option<(u32, f64)> = None;
                for x in series.raw.range(a, e) {
                    if let Some((req, pt)) = prev
                        && (req != x.request || x.t - pt > 0.5)
                    {
                        dec.gap();
                    }
                    dec.push(tf.pos(x.t, x.bps));
                    prev = Some((x.request, x.t));
                }
            }
            dec.finish();
            for (i, j) in segments(&self.breaks) {
                sc.area(&self.pts[i..j], plot.y1, color, palette::FILL_ALPHA);
            }
            for (i, j) in segments(&self.breaks) {
                sc.line(&self.pts[i..j], palette::DATA_LINE, color, 1.0);
            }
            // Percentiles of per-request throughput in view.
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
        let (tf, has_data, _) = self.throughput_tf(inp, view, w, h);
        let plot = tf.rect;
        let s = inp.s;
        if !has_data || !plot.contains(hp) {
            return Hover::default();
        }
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
        let mut best: Option<(f32, usize, usize)> = None;
        for (di, series) in s.throughput.iter().enumerate() {
            let i = series.raw.partition_point(|x| x.t < th);
            for j in [i.saturating_sub(1), i] {
                if let Some(x) = series.raw.get(j) {
                    let p = tf.pos(x.t, x.bps);
                    let d = (p.x - hp.x).abs() + 0.25 * (p.y - hp.y).abs();
                    if best.is_none_or(|(bd, _, _)| d < bd) {
                        best = Some((d, di, j));
                    }
                }
            }
        }
        let Some((_, di, j)) = best else {
            return Hover::default();
        };
        let Some(x) = s.throughput[di].raw.get(j).copied() else {
            return Hover::default();
        };
        let p = tf.pos(x.t, x.bps);
        if (p.x - hp.x).abs() > palette::SNAP_PX {
            return Hover::default();
        }
        let mut tip = match s.request(x.request) {
            Some(r) => Tip::default().title(format!(
                "{} {} · sample {}/{}",
                size_label(r.size),
                x.dir.lower(),
                r.index,
                r.count
            )),
            None => Tip::default().title(format!("{} · request in progress", x.dir.label())),
        };
        tip.push_pair("Throughput", format!("{} Mbps", format::mbps(x.bps)));
        tip.push_pair("Time", wall(inp, x.t));
        tip.push_pair(
            "Bin",
            format!("{} · {:.0} ms", x.bin + 1, f64::from(x.dt) * 1000.0),
        );
        if let Some(r) = s.request(x.request)
            && let Some(b) = r.bps
        {
            tip.push_pair("Request", format!("{} Mbps", format::mbps(b)));
        }
        let tip = tip.note(match x.dir {
            Direction::Download => "Body bytes received in the bin, divided by the bin length.",
            Direction::Upload => {
                "Bytes the server acknowledged in the bin (kernel TCP counters), divided by the bin length."
            }
        });
        Hover {
            crosshair: Some(p.x),
            highlight: Some((p, dir_color(x.dir))),
            tip: Some(tip),
            anchor: p,
        }
    }

    fn latency_stats(&mut self, s: &Session, view: &TimeView, t0: f64, t1: f64) -> (f64, Pct) {
        let (lat, gw) = (&s.latency, &s.gateway);
        let (la, lb) = visible(lat, t0, t1);
        let (ga, gb) = visible(gw, t0, t1);
        let vals = &mut self.vals;
        self.lat_stats.get(stats_key(s, view), || {
            let mut vmax: f64 = 0.0;
            for x in lat.raw.range(la, lb) {
                vmax = vmax.max(x.ms);
            }
            for x in gw.raw.range(ga, gb) {
                if let Some(m) = x.ms {
                    vmax = vmax.max(m);
                }
            }
            if let Some((tier, until)) = aged_tier(lat, t0, t1) {
                for bk in tier
                    .iter()
                    .filter(|bk| bk.t0 + tier.width > t0 && bk.t0 < until && bk.n > 0)
                {
                    vmax = vmax.max(bk.max);
                }
            }
            vals.clear();
            vals.extend(
                lat.raw
                    .range(la, lb)
                    .filter(|x| x.t >= t0 && x.t <= t1)
                    .map(|x| x.ms),
            );
            let pct = crate::stats::summarize(vals).map(|sm| (sm.median, sm.p90));
            (vmax, pct)
        })
    }

    fn latency_tf(
        &mut self,
        inp: &Inputs<'_>,
        view: &TimeView,
        w: f32,
        h: f32,
    ) -> (Transform, bool, Pct) {
        let (t0, t1) = view.range(inp.run_end);
        let (vmax, pct) = self.latency_stats(inp.s, view, t0, t1);
        let tf = Transform {
            rect: time_plot(w, h),
            t0,
            t1,
            v0: 0.0,
            v1: nice_max(vmax),
        };
        (tf, vmax > 0.0, pct)
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
        let (tf, has_data, pct) = self.latency_tf(inp, view, w, h);
        let plot = tf.rect;
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
                (plot.height() / palette::TICK_SPACING_Y) as usize,
                &mut self.ticks,
            );
            value_axis(sc, &tf, &self.ticks, &format::tick, "ms");
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
        let gap_limit = (3.5 * s.config.continuous.latency_interval.as_secs_f64()).max(2.0);
        let dense = (lb - la) as f32 > plot.width() / palette::DOT_MIN_SPACING;
        // Load periods: when a download or upload was running.
        let first = s.requests.partition_point(|r| r.t_end < t0);
        for r in s.requests.range(first, s.requests.len()) {
            if r.t_start > t1 {
                break;
            }
            let x0 = tf.x(r.t_start).max(plot.x0);
            let x1 = tf.x(r.t_end).min(plot.x1);
            if x1 > x0 {
                sc.fill(
                    Rect {
                        x0,
                        y0: plot.y0,
                        x1,
                        y1: plot.y1,
                    },
                    dir_color(r.dir),
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
                        y1: plot.y1,
                    },
                    Color::Text3,
                    palette::BAND_ALPHA,
                );
            }
        }
        // Gateway pings.
        sc.clip_x(Some((plot.x0, plot.x1)));
        let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
        if coarse(gb - ga, plot) {
            feed_buckets(&mut dec, tier_for(gw, t0), &tf);
        } else {
            let mut prev_t: Option<f64> = None;
            for x in gw.raw.range(ga, gb) {
                match x.ms {
                    Some(m) => {
                        if prev_t.is_some_and(|p| x.t - p > gap_limit) {
                            dec.gap();
                        }
                        dec.push(tf.pos(x.t, m));
                        prev_t = Some(x.t);
                    }
                    None => dec.gap(),
                }
            }
        }
        dec.finish();
        for (i, j) in segments(&self.breaks) {
            sc.line(&self.pts[i..j], palette::STROKE, GATEWAY, 1.0);
        }
        // Latency.
        let lat_coarse = coarse(lb - la, plot);
        if !lat_coarse && let Some((tier, until)) = aged_tier(lat, t0, t1) {
            draw_buckets(sc, tier, until, &tf, Color::Latency, &mut self.aux);
        }
        let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
        if lat_coarse {
            feed_buckets(&mut dec, tier_for(lat, t0), &tf);
        } else {
            let mut prev_t: Option<f64> = None;
            for x in lat.raw.range(la, lb) {
                if prev_t.is_some_and(|p| x.t - p > gap_limit) {
                    dec.gap();
                }
                dec.push(tf.pos(x.t, x.ms));
                prev_t = Some(x.t);
            }
        }
        dec.finish();
        for (i, j) in segments(&self.breaks) {
            sc.line(&self.pts[i..j], palette::DATA_LINE, Color::Latency, 1.0);
        }
        if !dense {
            self.aux.clear();
            self.aux
                .extend(lat.raw.range(la, lb).map(|x| tf.pos(x.t, x.ms)));
            sc.dots(&self.aux, palette::POINT_RADIUS, Color::Latency, 1.0);
        }
        sc.clip_x(None);
        // Lost UDP probes.
        loss_ticks(
            sc,
            s,
            &tf,
            plot.y1,
            plot.y1 - palette::LOSS_TICK,
            palette::DATA_LINE,
        );
        // Percentiles of visible latency.
        if let Some((p50, p90)) = pct {
            for v in [p50, p90] {
                sc.dashed_hline(
                    plot.x0,
                    plot.x1,
                    tf.y(v).round() + 0.5,
                    Color::Latency,
                    palette::PERCENTILE_ALPHA,
                );
            }
            let mut labels: Vec<(f32, String, Color)> = [("p50", p50), ("p90", p90)]
                .iter()
                .map(|(name, v)| {
                    (
                        tf.y(*v),
                        format!("{name} {} ms", format::ms(*v)),
                        Color::Latency,
                    )
                })
                .collect();
            percentile_labels(sc, plot, label_h, &mut labels);
        }
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
        let plot = tf.rect;
        let s = inp.s;
        if !plot.contains(hp) {
            return Hover::default();
        }
        let th = tf.t_at(hp.x);
        let (lat, gw) = (&s.latency, &s.gateway);

        // Loss ticks take the hover near the bottom edge.
        if hp.y > plot.y1 - palette::LOSS_TICK - palette::S4 {
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
        let lat_p = best_lat.map(|x| tf.pos(x.t, x.ms));
        let gw_p = best_gw.and_then(|g| g.ms.map(|m| tf.pos(g.t, m)));
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
        let tf = overview_tf(inp, w, h);
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
            // Raw bins when they're all present, aggregates otherwise.
            let mut dec = Decimator::new(&mut self.pts, &mut self.breaks);
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
        let tf = overview_tf(inp, w, h);
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
pub fn overview_tf(inp: &Inputs<'_>, w: f32, h: f32) -> Transform {
    let x = time_plot(w, h);
    let rect = Rect {
        x0: x.x0,
        y0: 0.0,
        x1: x.x1,
        y1: h,
    };
    let mut vmax: f64 = 0.0;
    for series in &inp.s.throughput {
        for b in series.minute.iter() {
            if b.n > 0 {
                vmax = vmax.max(b.max);
            }
        }
    }
    Transform {
        rect,
        t0: 0.0,
        t1: inp.run_end.max(1.0),
        v0: 0.0,
        v1: nice_max(vmax),
    }
}
