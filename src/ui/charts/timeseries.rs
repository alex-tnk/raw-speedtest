//! Throughput and latency over time, and the whole-run overview strip.

use std::time::{Duration, SystemTime};

use eframe::egui::{
    Align2, Color32, CornerRadius, Event, Id, MouseWheelUnit, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, pos2,
};

use super::{Decimator, MeshCache, TimeView, Transform, key_of, nice_max, nice_ticks, segments};
use crate::app::session::{RunState, Session};
use crate::engine::size_label;
use crate::model::{Direction, ProbeOutcome};
use crate::stats::{Bucket, Sample, Series, Tier};
use crate::ui::format;
use crate::ui::text::{self, Text};
use crate::ui::theme;
use crate::ui::widgets::{Tip, tooltip};

/// A value recomputed only when its key changes.
#[derive(Default)]
struct Memo<T> {
    key: Option<u64>,
    val: T,
}

impl<T: Copy> Memo<T> {
    fn get(&mut self, key: u64, f: impl FnOnce() -> T) -> T {
        if self.key != Some(key) {
            self.val = f();
            self.key = Some(key);
        }
        self.val
    }
}

type Pct = Option<(f64, f64)>;

#[derive(Default)]
pub struct Charts {
    /// Axis maximum and per-direction (p50, p90) for the view.
    tput_stats: Memo<(f64, [Pct; 2])>,
    /// Axis maximum and (p50, p90) of latency for the view.
    lat_stats: Memo<(f64, Pct)>,
    tput: MeshCache,
    lat: MeshCache,
    overview: MeshCache,
    pts: Vec<Pos2>,
    /// Second point buffer for aggregates drawn while `pts` is in use.
    aux: Vec<Pos2>,
    breaks: Vec<usize>,
    ticks: Vec<f64>,
    vals: Vec<f64>,
}

pub struct Inputs<'a> {
    pub s: &'a Session,
    pub text: &'a Text,
    pub wall0: SystemTime,
    pub run_end: f64,
}

fn dir_color(d: Direction) -> Color32 {
    match d {
        Direction::Download => theme::DOWNLOAD,
        Direction::Upload => theme::UPLOAD,
    }
}

/// Visible raw index range, or whole series.
fn visible<T: Sample>(series: &Series<T>, t0: f64, t1: f64) -> (usize, usize) {
    let (a, b) = series.raw_range(t0, t1);
    // One sample of margin on each side so lines run to the plot edges.
    (a.saturating_sub(1), (b + 1).min(series.raw.len()))
}

/// The tier to draw where raw samples have been evicted, if any part of
/// `[t0, raw_start)` is visible.
fn aged_tier<T: Sample>(series: &Series<T>, t0: f64, t1: f64) -> Option<(&Tier, f64)> {
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
    b: &mut super::Builder<'_>,
    tier: &Tier,
    until: f64,
    tf: &Transform,
    color: Color32,
    pts: &mut Vec<Pos2>,
) {
    pts.clear();
    let start = tier.first_ending_after(tf.t0);
    let mut i = start;
    while let Some(bk) = tier.get(i) {
        if bk.t0 > tf.t1 || bk.t0 >= until {
            break;
        }
        if let Some(mean) = bk.mean() {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            b.segment(
                pos2(x, tf.y(bk.min)),
                pos2(x, tf.y(bk.max)),
                theme::STROKE,
                theme::tint(color, theme::ENVELOPE_ALPHA),
            );
            pts.push(pos2(x, tf.y(mean)));
        }
        i += 1;
    }
    b.polyline(pts, theme::DATA_LINE, color);
}

/// True when the view holds far more raw samples than pixels. The chart
/// then draws from per-minute min/max aggregates: at that zoom the M4
/// envelope is the same, and building it doesn't walk every raw sample.
fn coarse(visible_samples: usize, plot: Rect) -> bool {
    visible_samples as f32 > 8.0 * plot.width()
}

/// The finest tier that still covers `t0`.
fn tier_for<T: Sample>(series: &Series<T>, t0: f64) -> &Tier {
    match series.minute.get(0) {
        Some(b) if b.t0 <= t0 => &series.minute,
        _ => &series.hour,
    }
}

/// Loss ticks: one per pixel column holding a lost probe. Zoomed far out,
/// read per-minute miss counts instead of every probe.
fn loss_ticks(
    b: &mut super::Builder<'_>,
    s: &Session,
    tf: &Transform,
    y0: f32,
    y1: f32,
    width: f32,
) {
    let (pa, pb) = s.loss.raw_range(tf.t0, tf.t1);
    let mut last_x = f32::NEG_INFINITY;
    let mut tick = |b: &mut super::Builder<'_>, t: f64| {
        let x = tf.x(t).round() + 0.5;
        if x - last_x >= 1.0 {
            b.segment(pos2(x, y0), pos2(x, y1), width, theme::LOSS);
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
                tick(b, bk.t0 + tier.width / 2.0);
            }
            i += 1;
        }
    } else {
        for p in s.loss.raw.range(pa, pb) {
            if p.outcome == ProbeOutcome::Lost {
                tick(b, p.t);
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

/// Pan and zoom from pointer input over a chart.
fn interact_time(ui: &Ui, resp: &Response, tf: &Transform, view: &mut TimeView, run_end: f64) {
    let secs_per_px = (tf.t1 - tf.t0) / f64::from(tf.rect.width().max(1.0));
    if resp.double_clicked() {
        *view = TimeView::default();
        return;
    }
    if resp.dragged() {
        let dx = resp.drag_delta().x;
        if dx != 0.0 {
            if view.span.is_none() {
                return;
            }
            view.pan(-f64::from(dx) * secs_per_px, run_end);
        }
    }
    if !resp.hovered() {
        return;
    }
    let (zoom, pan_px, anchor) = ui.input(|i| {
        let mut pan = 0.0f32;
        for e in &i.events {
            if let Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = e
            {
                if modifiers.ctrl || modifiers.command {
                    continue;
                }
                let k = match unit {
                    MouseWheelUnit::Point => 1.0,
                    MouseWheelUnit::Line => theme::WHEEL_LINE,
                    MouseWheelUnit::Page => tf.rect.width(),
                };
                pan += delta.x * k;
            }
        }
        (i.zoom_delta(), pan, i.pointer.hover_pos())
    });
    if (zoom - 1.0).abs() > 1e-4 {
        let at = anchor.map_or(tf.t1, |p| tf.t_at(p.x));
        view.zoom(f64::from(zoom), at, run_end);
    }
    if pan_px != 0.0 && view.span.is_some() {
        view.pan(-f64::from(pan_px) * secs_per_px, run_end);
    }
}

fn crosshair(ui: &Ui, plot: Rect, x: f32) {
    ui.painter().vline(
        x.round() + 0.5,
        plot.y_range(),
        Stroke::new(theme::STROKE, theme::TEXT_3),
    );
}

fn highlight(ui: &Ui, p: Pos2, color: Color32) {
    ui.painter().circle(
        p,
        theme::POINT_HOVER_RADIUS,
        color,
        Stroke::new(theme::STROKE, theme::BG),
    );
}

fn legend(ui: &Ui, text: &Text, at: Pos2, items: &[(&str, Color32, bool)]) {
    let p = ui.painter();
    let mut x = at.x;
    for (label, color, dashed_tick) in items {
        let y = at.y;
        if *dashed_tick {
            p.vline(
                x + theme::LOSS_TICK / 2.0,
                (y - theme::LOSS_TICK / 2.0)..=(y + theme::LOSS_TICK / 2.0),
                Stroke::new(theme::DATA_LINE, *color),
            );
            x += theme::LOSS_TICK + theme::S4;
        } else {
            p.hline(
                x..=(x + theme::LEGEND_SWATCH),
                y,
                Stroke::new(theme::DATA_LINE, *color),
            );
            x += theme::LEGEND_SWATCH + theme::S4;
        }
        let r = text.paint(p, pos2(x, y), Align2::LEFT_CENTER, label, text::SMALL_2);
        x = r.right() + theme::S12;
    }
}

/// Tooltip for the download/upload band at time `t`, if any.
fn band_tip(ui: &Ui, inp: &Inputs<'_>, t: f64, at: Pos2) {
    let s = inp.s;
    if let Some(&(p0, p1)) = s
        .pauses
        .iter()
        .find(|(p0, p1)| *p0 <= t && p1.unwrap_or(inp.run_end) >= t)
    {
        let mut tip = Tip::default().title("Paused");
        tip.push_pair("From", wall(inp, p0));
        tip.push_pair("To", p1.map_or("now".to_owned(), |p| wall(inp, p)));
        tooltip(ui, inp.text, at, &tip);
        return;
    }
    let i = s.requests.partition_point(|r| r.t_end < t);
    let Some(r) = s.requests.get(i).filter(|r| r.t_start <= t) else {
        return;
    };
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
    let tip = tip.note("Shaded while a download or upload request is in flight.");
    tooltip(ui, inp.text, at, &tip);
}

fn wall(inp: &Inputs<'_>, t: f64) -> String {
    format::clock(inp.wall0 + Duration::from_secs_f64(t.max(0.0)), true)
}

impl Charts {
    /// Throughput chart. `rect` includes the axes.
    pub fn throughput(&mut self, ui: &mut Ui, rect: Rect, inp: &Inputs<'_>, view: &mut TimeView) {
        let plot = Rect::from_min_max(
            pos2(rect.left() + theme::AXIS_WIDTH, rect.top() + theme::S16),
            pos2(
                rect.right() - theme::RIGHT_GUTTER,
                rect.bottom() - theme::AXIS_HEIGHT,
            ),
        );
        let (t0, t1) = view.range(inp.run_end);
        let s = inp.s;
        // Value range and percentiles over everything visible, recomputed
        // only when the data or the view changes.
        let vals = &mut self.vals;
        let (vmax, pct) =
            self.tput_stats
                .get(key_of((s.version, t0.to_bits(), t1.to_bits())), || {
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
                });
        let has_data = vmax > 0.0;
        let v1 = nice_max(vmax);
        let tf = Transform {
            rect: plot,
            t0,
            t1,
            v0: 0.0,
            v1,
        };
        let painter = ui.painter_at(rect.expand(theme::S8));
        legend(
            ui,
            inp.text,
            pos2(plot.left(), rect.top() + theme::S4),
            &[
                ("Download", theme::DOWNLOAD, false),
                ("Upload", theme::UPLOAD, false),
            ],
        );
        if has_data {
            nice_ticks(
                0.0,
                v1,
                (plot.height() / theme::TICK_SPACING_Y) as usize,
                &mut self.ticks,
            );
            super::value_axis(
                &painter,
                inp.text,
                plot,
                &tf,
                &self.ticks,
                &format::si,
                "bps",
            );
        }
        painter.hline(
            plot.x_range(),
            plot.bottom().round() + 0.5,
            Stroke::new(theme::STROKE, theme::BORDER),
        );
        if s.wall_start.is_some() {
            super::time_axis(&painter, inp.text, plot, &tf, inp.wall0, &mut self.ticks);
        }

        let key = key_of((
            s.version,
            plot.min.x.to_bits(),
            plot.min.y.to_bits(),
            plot.max.x.to_bits(),
            plot.max.y.to_bits(),
            t0.to_bits(),
            t1.to_bits(),
            v1.to_bits(),
        ));
        let (pts, breaks, aux) = (&mut self.pts, &mut self.breaks, &mut self.aux);
        let clip = painter.clone();
        self.tput.paint(ui, &clip, key, |b| {
            for (di, dir) in [Direction::Download, Direction::Upload]
                .into_iter()
                .enumerate()
            {
                let series = &s.throughput[di];
                let color = dir_color(dir);
                let (a, e) = visible(series, t0, t1);
                let mut dec = Decimator::new(pts, breaks);
                if coarse(e - a, plot) {
                    feed_buckets(&mut dec, tier_for(series, t0), &tf);
                } else {
                    if let Some((tier, until)) = aged_tier(series, t0, t1) {
                        draw_buckets(b, tier, until, &tf, color, aux);
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
                for (i, j) in segments(breaks) {
                    b.fill_under(
                        &pts[i..j],
                        plot.bottom(),
                        theme::tint(color, theme::FILL_ALPHA),
                    );
                }
                for (i, j) in segments(breaks) {
                    b.polyline(&pts[i..j], theme::DATA_LINE, color);
                }
                // Percentiles of per-request throughput in view.
                if let Some((p50, p90)) = pct[di] {
                    for v in [p50, p90] {
                        let y = tf.y(v).round() + 0.5;
                        b.dashed_hline(
                            plot.left(),
                            plot.right(),
                            y,
                            theme::tint(color, theme::PERCENTILE_ALPHA),
                        );
                    }
                }
            }
        });
        // Percentile labels (text is cheap; drawn each frame).
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
        super::percentile_labels(&painter, inp.text, plot, &mut labels);

        let resp = ui.interact(plot, Id::unique("tput-plot"), Sense::click_and_drag());
        interact_time(ui, &resp, &tf, view, inp.run_end);
        let Some(hp) = resp.hover_pos() else { return };
        if !has_data {
            return;
        }
        let th = tf.t_at(hp.x);
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
        if let Some((tier, until)) =
            aged_tier(&s.throughput[0], t0, t1).or(aged_tier(&s.throughput[1], t0, t1))
            && th < until
            && let Some(bk) = bucket_at(tier, th)
        {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            crosshair(ui, plot, x);
            let tip = Tip::default()
                        .title(format!("{} aggregate", if tier.width >= 3600.0 { "Hour" } else { "Minute" }))
                        .pair("Start", wall(inp, bk.t0))
                        .pair("Bins", format::count(u64::from(bk.n)))
                        .pair("Min", format!("{} Mbps", format::mbps(bk.min)))
                        .pair("Mean", format!("{} Mbps", format::mbps(bk.mean().unwrap_or(0.0))))
                        .pair("Max", format!("{} Mbps", format::mbps(bk.max)))
                        .note("Raw bins older than the sample buffer are kept as per-minute and per-hour aggregates.");
            tooltip(ui, inp.text, pos2(x, hp.y), &tip);
            return;
        }
        let Some((_, di, j)) = best else { return };
        let x = *s.throughput[di].raw.get(j).expect("index from search");
        let p = tf.pos(x.t, x.bps);
        if (p.x - hp.x).abs() > theme::SNAP_PX {
            return;
        }
        crosshair(ui, plot, p.x);
        highlight(ui, p, dir_color(x.dir));
        let mut tip = Tip::default();
        match s.request(x.request) {
            Some(r) => {
                tip = tip.title(format!(
                    "{} download · sample {}/{}",
                    size_label(r.size),
                    r.index,
                    r.count
                ));
                if x.dir == Direction::Upload {
                    tip = Tip::default().title(format!(
                        "{} upload · sample {}/{}",
                        size_label(r.size),
                        r.index,
                        r.count
                    ));
                }
            }
            None => tip = tip.title(format!("{} · request in progress", x.dir.label())),
        }
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
        tooltip(ui, inp.text, p, &tip);
    }

    /// Latency over time with gateway pings, load periods and loss ticks.
    pub fn latency(&mut self, ui: &mut Ui, rect: Rect, inp: &Inputs<'_>, view: &mut TimeView) {
        let plot = Rect::from_min_max(
            pos2(rect.left() + theme::AXIS_WIDTH, rect.top() + theme::S16),
            pos2(
                rect.right() - theme::RIGHT_GUTTER,
                rect.bottom() - theme::AXIS_HEIGHT,
            ),
        );
        let (t0, t1) = view.range(inp.run_end);
        let s = inp.s;
        let lat = &s.latency;
        let gw = &s.gateway;
        let (la, lb) = visible(lat, t0, t1);
        let (ga, gb) = visible(gw, t0, t1);
        let vals = &mut self.vals;
        let (vmax, pct) =
            self.lat_stats
                .get(key_of((s.version, t0.to_bits(), t1.to_bits())), || {
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
                });
        let has_data = vmax > 0.0;
        let v1 = nice_max(vmax);
        let tf = Transform {
            rect: plot,
            t0,
            t1,
            v0: 0.0,
            v1,
        };
        let painter = ui.painter_at(rect.expand(theme::S8));
        legend(
            ui,
            inp.text,
            pos2(plot.left(), rect.top() + theme::S4),
            &[
                ("Latency", theme::LATENCY, false),
                ("Gateway", theme::GATEWAY, false),
                ("UDP probe lost", theme::LOSS, true),
            ],
        );
        if has_data {
            nice_ticks(
                0.0,
                v1,
                (plot.height() / theme::TICK_SPACING_Y) as usize,
                &mut self.ticks,
            );
            super::value_axis(
                &painter,
                inp.text,
                plot,
                &tf,
                &self.ticks,
                &format::tick,
                "ms",
            );
        }
        painter.hline(
            plot.x_range(),
            plot.bottom().round() + 0.5,
            Stroke::new(theme::STROKE, theme::BORDER),
        );
        if s.wall_start.is_some() {
            super::time_axis(&painter, inp.text, plot, &tf, inp.wall0, &mut self.ticks);
        }
        let gap_limit = (3.5 * s.config.continuous.latency_interval.as_secs_f64()).max(2.0);
        let dense = (lb - la) as f32 > plot.width() / theme::DOT_MIN_SPACING;
        let key = key_of((
            s.version,
            plot.min.x.to_bits(),
            plot.min.y.to_bits(),
            plot.max.x.to_bits(),
            plot.max.y.to_bits(),
            t0.to_bits(),
            t1.to_bits(),
            v1.to_bits(),
            // An ongoing pause band grows with the clock, not with data.
            (s.state == RunState::Paused).then_some(inp.run_end as u64),
        ));
        let (pts, breaks) = (&mut self.pts, &mut self.breaks);
        let clip = painter.clone();
        self.lat.paint(ui, &clip, key, |b| {
            // Load periods: when a download or upload was running.
            let first = s.requests.partition_point(|r| r.t_end < t0);
            for r in s.requests.range(first, s.requests.len()) {
                if r.t_start > t1 {
                    break;
                }
                let x0 = tf.x(r.t_start).max(plot.left());
                let x1 = tf.x(r.t_end).min(plot.right());
                if x1 > x0 {
                    b.rect(
                        Rect::from_x_y_ranges(x0..=x1, plot.y_range()),
                        theme::tint(dir_color(r.dir), theme::BAND_ALPHA),
                    );
                }
            }
            // Paused periods: nothing was measured.
            let run_end = inp.run_end;
            for &(p0, p1) in s.pauses.iter() {
                let p1 = p1.unwrap_or(run_end);
                let x0 = tf.x(p0).max(plot.left());
                let x1 = tf.x(p1).min(plot.right());
                if x1 > x0 {
                    b.rect(
                        Rect::from_x_y_ranges(x0..=x1, plot.y_range()),
                        theme::tint(theme::TEXT_3, theme::BAND_ALPHA),
                    );
                }
            }
            // Gateway pings.
            let mut dec = Decimator::new(pts, breaks);
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
            for (i, j) in segments(breaks) {
                b.polyline(&pts[i..j], theme::STROKE, theme::GATEWAY);
            }
            // Latency.
            let lat_coarse = coarse(lb - la, plot);
            if !lat_coarse && let Some((tier, until)) = aged_tier(lat, t0, t1) {
                draw_buckets(b, tier, until, &tf, theme::LATENCY, pts);
            }
            let mut dec = Decimator::new(pts, breaks);
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
            for (i, j) in segments(breaks) {
                b.polyline(&pts[i..j], theme::DATA_LINE, theme::LATENCY);
            }
            if !dense {
                for x in lat.raw.range(la, lb) {
                    b.dot(tf.pos(x.t, x.ms), theme::POINT_RADIUS, theme::LATENCY);
                }
            }
            // Lost UDP probes.
            loss_ticks(
                b,
                s,
                &tf,
                plot.bottom(),
                plot.bottom() - theme::LOSS_TICK,
                theme::DATA_LINE,
            );
            // Percentiles of visible latency.
            if let Some((p50, p90)) = pct {
                for v in [p50, p90] {
                    b.dashed_hline(
                        plot.left(),
                        plot.right(),
                        tf.y(v).round() + 0.5,
                        theme::tint(theme::LATENCY, theme::PERCENTILE_ALPHA),
                    );
                }
            }
        });
        if let Some((p50, p90)) = pct {
            let mut labels: Vec<(f32, String, Color32)> = [("p50", p50), ("p90", p90)]
                .iter()
                .map(|(name, v)| {
                    (
                        tf.y(*v),
                        format!("{name} {} ms", format::ms(*v)),
                        theme::LATENCY,
                    )
                })
                .collect();
            super::percentile_labels(&painter, inp.text, plot, &mut labels);
        }

        let resp = ui.interact(plot, Id::unique("lat-plot"), Sense::click_and_drag());
        interact_time(ui, &resp, &tf, view, inp.run_end);
        let Some(hp) = resp.hover_pos() else { return };
        let th = tf.t_at(hp.x);

        // Loss ticks take the hover near the bottom edge.
        if hp.y > plot.bottom() - theme::LOSS_TICK - theme::S4 {
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
                if (x - hp.x).abs() <= theme::HOVER_PX {
                    crosshair(ui, plot, x);
                    let tip = Tip::default()
                        .title(format!("UDP probe {} lost · {}", p.seq, p.load.label().to_lowercase()))
                        .pair("Sent", wall(inp, p.t))
                        .note("STUN Binding request to stun.cloudflare.com with no answer within 3 s.");
                    tooltip(
                        ui,
                        inp.text,
                        pos2(x, plot.bottom() - theme::LOSS_TICK),
                        &tip,
                    );
                    return;
                }
            }
        }
        if !has_data {
            return;
        }
        if let Some((tier, until)) = aged_tier(lat, t0, t1)
            && th < until
            && let Some(bk) = bucket_at(tier, th)
        {
            let x = tf.x(bk.t0 + tier.width / 2.0);
            crosshair(ui, plot, x);
            let mut tip = Tip::default()
                .title(format!(
                    "{} aggregate",
                    if tier.width >= 3600.0 {
                        "Hour"
                    } else {
                        "Minute"
                    }
                ))
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
            tooltip(ui, inp.text, pos2(x, hp.y), &tip);
            return;
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
        let score = |p: Pos2| (p.x - hp.x).abs() + (p.y - hp.y).abs() * 0.5;
        let use_gw = match (lat_p, gw_p) {
            (Some(a), Some(g)) => score(g) < score(a),
            (None, Some(_)) => true,
            _ => false,
        };
        let near = |p: Option<Pos2>| p.is_some_and(|p| (p.x - hp.x).abs() <= theme::SNAP_PX);
        if !near(lat_p) && !near(gw_p) {
            // No probe nearby: describe the load band under the pointer.
            band_tip(ui, inp, th, pos2(hp.x, hp.y));
            return;
        }
        if use_gw {
            let (g, p) = (best_gw.expect("checked"), gw_p.expect("checked"));
            if (p.x - hp.x).abs() > theme::SNAP_PX {
                return;
            }
            crosshair(ui, plot, p.x);
            highlight(ui, p, theme::GATEWAY);
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
            tooltip(ui, inp.text, p, &tip);
            return;
        }
        let (Some(x), Some(p)) = (best_lat, lat_p) else {
            return;
        };
        if (p.x - hp.x).abs() > theme::SNAP_PX {
            return;
        }
        crosshair(ui, plot, p.x);
        highlight(ui, p, theme::LATENCY);
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
        tooltip(ui, inp.text, p, &tip);
    }

    /// Whole-run strip with the visible window marked. Click or drag to
    /// move the window; double-click to show everything.
    pub fn overview(&mut self, ui: &mut Ui, rect: Rect, inp: &Inputs<'_>, view: &mut TimeView) {
        let plot = Rect::from_min_max(
            pos2(rect.left() + theme::AXIS_WIDTH, rect.top()),
            pos2(rect.right() - theme::RIGHT_GUTTER, rect.bottom()),
        );
        let s = inp.s;
        let run_end = inp.run_end.max(1.0);
        let mut vmax: f64 = 0.0;
        for series in &s.throughput {
            for b in series.minute.iter() {
                if b.n > 0 {
                    vmax = vmax.max(b.max);
                }
            }
        }
        let tf = Transform {
            rect: plot,
            t0: 0.0,
            t1: run_end,
            v0: 0.0,
            v1: nice_max(vmax),
        };
        let painter = ui.painter_at(rect);
        painter.rect_stroke(
            plot,
            CornerRadius::same(theme::RADIUS),
            Stroke::new(theme::STROKE, theme::BORDER),
            StrokeKind::Inside,
        );
        // Rebuilt at most once per second of run time or 64 data changes.
        let key = key_of((
            s.version / 64,
            plot.min.x.to_bits(),
            plot.max.x.to_bits(),
            plot.min.y.to_bits(),
            run_end as u64,
            s.state as u8,
        ));
        let (pts, breaks) = (&mut self.pts, &mut self.breaks);
        self.overview.paint(ui, &painter, key, |b| {
            for (di, series) in s.throughput.iter().enumerate() {
                let color = if di == 0 {
                    theme::DOWNLOAD
                } else {
                    theme::UPLOAD
                };
                // Raw bins when they're all present, aggregates otherwise.
                let mut dec = Decimator::new(pts, breaks);
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
                    for bk in series.minute.iter() {
                        match bk.mean() {
                            Some(_) => dec.push(tf.pos(bk.t0 + 30.0, bk.max)),
                            None => dec.gap(),
                        }
                    }
                }
                dec.finish();
                for (i, j) in segments(breaks) {
                    b.fill_under(
                        &pts[i..j],
                        plot.bottom() - 1.0,
                        theme::tint(color, theme::OVERVIEW_FILL_ALPHA),
                    );
                }
            }
            loss_ticks(
                b,
                s,
                &tf,
                plot.bottom() - 1.0,
                plot.bottom() - theme::S8,
                theme::STROKE,
            );
        });
        // Visible window.
        let (t0, t1) = view.range(run_end);
        if view.span.is_some() {
            let w = Rect::from_x_y_ranges(tf.x(t0)..=tf.x(t1).max(tf.x(t0) + 2.0), plot.y_range());
            let dim = theme::tint(theme::BG, theme::OUTSIDE_VIEW_ALPHA);
            painter.rect_filled(
                Rect::from_x_y_ranges(plot.left()..=w.left(), plot.y_range()),
                CornerRadius::ZERO,
                dim,
            );
            painter.rect_filled(
                Rect::from_x_y_ranges(w.right()..=plot.right(), plot.y_range()),
                CornerRadius::ZERO,
                dim,
            );
            painter.rect_stroke(
                w,
                CornerRadius::ZERO,
                Stroke::new(theme::STROKE, theme::ACCENT),
                StrokeKind::Inside,
            );
        }
        if s.wall_start.is_some() {
            inp.text.paint(
                &painter,
                pos2(plot.right() + theme::S8, plot.center().y),
                Align2::LEFT_CENTER,
                &format::elapsed(run_end),
                text::SMALL_2,
            );
        }
        let resp = ui.interact(plot, Id::unique("overview"), Sense::click_and_drag());
        if resp.double_clicked() {
            *view = TimeView::default();
        } else if let Some(p) = resp
            .interact_pointer_pos()
            .filter(|_| resp.clicked() || resp.dragged())
        {
            view.center(tf.t_at(p.x), run_end);
        }
        if let Some(hp) = resp.hover_pos().filter(|_| s.wall_start.is_some()) {
            let t = tf.t_at(hp.x);
            let tip = Tip::default()
                .title("Whole run")
                .pair("Time", wall(inp, t))
                .pair("Elapsed", format::elapsed(t));
            tooltip(ui, inp.text, hp, &tip);
        }
    }
}
