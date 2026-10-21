//! Chart building blocks: the shared time window, axes, decimation for
//! display, and the hover result.

pub mod boxplot;
pub mod rssi;
pub mod series;

use std::time::{Duration, SystemTime};

use super::format;
use super::palette::{self, Color};
use super::scene::{HAlign, Point, Rect, Scene, TextSize, VAlign, pt};
use super::tip::Tip;

/// What hovering a chart shows. Coordinates are chart-local points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hover {
    /// x of a vertical crosshair across the plot.
    pub crosshair: Option<f32>,
    /// A ringed point in the given colour.
    pub highlight: Option<(Point, Color)>,
    pub tip: Option<Tip>,
    /// Where the tooltip attaches; it must not cover this point.
    pub anchor: Point,
}

/// The visible time range of the time-series charts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeView {
    /// Visible span in seconds; `None` fits the whole run.
    pub span: Option<f64>,
    /// End of the window; `None` follows the newest data.
    pub end: Option<f64>,
}

impl TimeView {
    /// `(t0, t1)` for a run whose data ends at `run_end`.
    pub fn range(&self, run_end: f64) -> (f64, f64) {
        let run_end = run_end.max(1.0);
        match self.span {
            None => (0.0, run_end),
            Some(span) => {
                let end = self.end.unwrap_or(run_end).min(run_end);
                let start = (end - span).max(0.0);
                (start, (start + span).max(end))
            }
        }
    }

    #[cfg(test)]
    pub fn following(&self) -> bool {
        self.end.is_none()
    }

    /// Zoom by `factor` (> 1 zooms in) keeping time `anchor` fixed.
    pub fn zoom(&mut self, factor: f64, anchor: f64, run_end: f64) {
        let (t0, t1) = self.range(run_end);
        let span = ((t1 - t0) / factor).clamp(0.2, run_end.max(1.0));
        if span >= run_end.max(1.0) - 1e-9 {
            *self = TimeView::default();
            return;
        }
        let frac = ((anchor - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let start = (anchor - frac * span).max(0.0);
        let end = (start + span).min(run_end);
        self.span = Some(span);
        // Keep following when the right edge is at the newest data.
        self.end = if (end - run_end).abs() < span * 0.01 {
            None
        } else {
            Some(end)
        };
    }

    pub fn pan(&mut self, dt: f64, run_end: f64) {
        let (t0, t1) = self.range(run_end);
        let span = t1 - t0;
        if self.span.is_none() {
            return;
        }
        let end = (t1 + dt).clamp(span.min(run_end), run_end);
        self.end = if end >= run_end - span * 0.01 {
            None
        } else {
            Some(end)
        };
    }

    /// Center the window on `t` (overview strip clicks).
    pub fn center(&mut self, t: f64, run_end: f64) {
        let (t0, t1) = self.range(run_end);
        let span = t1 - t0;
        if self.span.is_none() {
            return;
        }
        let end = (t + span / 2.0).clamp(span.min(run_end), run_end);
        self.end = if end >= run_end - span * 0.01 {
            None
        } else {
            Some(end)
        };
    }
}

/// The plot area of a time-series chart of size `w`×`h`: value labels on
/// the left, percentile labels on the right, time labels below. The
/// overview strip uses the same horizontal extent, so the three charts
/// line up and share x coordinates.
pub fn time_plot(w: f32, h: f32) -> Rect {
    Rect {
        x0: palette::AXIS_WIDTH,
        y0: palette::PLOT_TOP,
        x1: (w - palette::RIGHT_GUTTER).max(palette::AXIS_WIDTH + 1.0),
        y1: (h - palette::AXIS_HEIGHT).max(palette::PLOT_TOP + 1.0),
    }
}

/// Maps data to chart points.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub rect: Rect,
    pub t0: f64,
    pub t1: f64,
    pub v0: f64,
    pub v1: f64,
}

impl Transform {
    pub fn x(&self, t: f64) -> f32 {
        let f = (t - self.t0) / (self.t1 - self.t0).max(1e-9);
        self.rect.x0 + (f as f32) * self.rect.width()
    }
    pub fn y(&self, v: f64) -> f32 {
        let f = (v - self.v0) / (self.v1 - self.v0).max(1e-12);
        self.rect.y1 - (f as f32) * self.rect.height()
    }
    pub fn t_at(&self, x: f32) -> f64 {
        self.t0 + f64::from((x - self.rect.x0) / self.rect.width().max(1.0)) * (self.t1 - self.t0)
    }
    pub fn pos(&self, t: f64, v: f64) -> Point {
        pt(self.x(t), self.y(v))
    }
    /// `y`, with values above the axis drawn on its top edge.
    pub fn yc(&self, v: f64) -> f32 {
        self.y(v.min(self.v1))
    }
    pub fn posc(&self, t: f64, v: f64) -> Point {
        pt(self.x(t), self.yc(v))
    }
}

/// Above this multiple of the robust top, rare spikes no longer set the
/// value axis: it fits the 99th percentile with some headroom, taller
/// samples are drawn on its top edge, and the gutter labels their maximum.
const SPIKE_RATIO: f64 = 1.5;
const ROBUST_HEADROOM: f64 = 1.25;
/// With fewer visible samples the axis always fits the maximum.
const ROBUST_MIN_SAMPLES: usize = 50;

/// Top of a value axis for samples whose maximum is `max` and 99th
/// percentile `p99` over `n` samples.
pub fn robust_top(max: f64, p99: Option<f64>, n: usize) -> f64 {
    match p99 {
        Some(p)
            if n >= ROBUST_MIN_SAMPLES && p > 0.0 && max > SPIKE_RATIO * p * ROBUST_HEADROOM =>
        {
            nice_max(p * ROBUST_HEADROOM)
        }
        _ => nice_max(max),
    }
}

/// 1-2-2.5-5 steps: ticks covering `[lo, hi]`, at most about `max_ticks`.
pub fn nice_ticks(lo: f64, hi: f64, max_ticks: usize, out: &mut Vec<f64>) {
    out.clear();
    if hi.is_nan() || lo.is_nan() || hi <= lo || max_ticks == 0 {
        return;
    }
    let raw = (hi - lo) / max_ticks.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    let mut v = (lo / step).ceil() * step;
    while v <= hi + step * 1e-9 {
        out.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
        v += step;
    }
}

/// Upper bound for a value axis: the next "nice" number above `max`.
pub fn nice_max(max: f64) -> f64 {
    if max.is_nan() || max <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(max.log10().floor());
    for m in [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0] {
        if m * mag >= max * 1.04 {
            return m * mag;
        }
    }
    10.0 * mag
}

const TIME_STEPS: [f64; 20] = [
    0.1,
    0.2,
    0.5,
    1.0,
    2.0,
    5.0,
    10.0,
    15.0,
    30.0,
    60.0,
    120.0,
    300.0,
    600.0,
    900.0,
    1800.0,
    3600.0,
    7200.0,
    3.0 * 3600.0,
    6.0 * 3600.0,
    12.0 * 3600.0,
];

/// Wall-clock aligned ticks for the run-relative range `[t0, t1]`.
pub fn time_ticks(t0: f64, t1: f64, width: f32, wall0: SystemTime, out: &mut Vec<f64>) -> f64 {
    out.clear();
    let max_ticks = (width / palette::TICK_SPACING_X).max(1.0) as f64;
    let span = (t1 - t0).max(1e-3);
    let step = TIME_STEPS
        .iter()
        .copied()
        .find(|s| span / s <= max_ticks)
        .unwrap_or(86_400.0);
    let wall0_s = wall0
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let offset = crate::platform::current()
        .utc_offset(wall0_s as i64)
        .map_or(0.0, f64::from);
    // Run time t has local wall time wall0_s + offset + t.
    let base = wall0_s + offset;
    let mut k = ((base + t0) / step).ceil();
    loop {
        let t = k * step - base;
        if t > t1 {
            break;
        }
        out.push(t);
        k += 1.0;
    }
    step
}

pub fn time_label(wall0: SystemTime, t: f64, step: f64) -> String {
    let w = wall0 + Duration::from_secs_f64(t.max(0.0));
    if step >= 60.0 {
        format::clock_hm(w)
    } else if step >= 1.0 {
        format::clock(w, false)
    } else {
        format::clock_tenths(w)
    }
}

/// Horizontal gridlines with value labels left of the plot, plus an
/// optional unit label above the axis.
pub fn value_axis(
    sc: &mut Scene,
    tf: &Transform,
    ticks: &[f64],
    label: &dyn Fn(f64) -> String,
    unit: &str,
) {
    let plot = tf.rect;
    for &v in ticks {
        let y = tf.y(v).round() + 0.5;
        if y < plot.y0 - 0.5 || y > plot.y1 + 0.5 {
            continue;
        }
        sc.segment(
            pt(plot.x0, y),
            pt(plot.x1, y),
            palette::STROKE,
            Color::Grid,
            1.0,
        );
        sc.text(
            pt(plot.x0 - palette::S8, y),
            &label(v),
            TextSize::Small,
            Color::Text2,
            HAlign::End,
            VAlign::Center,
        );
    }
    if !unit.is_empty() {
        sc.text(
            pt(plot.x0 - palette::S8, plot.y0 - palette::S8),
            unit,
            TextSize::Small,
            Color::Text2,
            HAlign::End,
            VAlign::Bottom,
        );
    }
}

/// Time tick marks and labels under the plot.
pub fn time_axis(sc: &mut Scene, tf: &Transform, wall0: SystemTime, scratch: &mut Vec<f64>) {
    let plot = tf.rect;
    let step = time_ticks(tf.t0, tf.t1, plot.width(), wall0, scratch);
    for &t in scratch.iter() {
        let x = tf.x(t).round() + 0.5;
        sc.segment(
            pt(x, plot.y1),
            pt(x, plot.y1 + palette::S4),
            palette::STROKE,
            Color::Grid,
            1.0,
        );
        sc.text(
            pt(tf.x(t), plot.y1 + palette::S4),
            &time_label(wall0, t, step),
            TextSize::Small,
            Color::Text2,
            HAlign::Center,
            VAlign::Top,
        );
    }
}

/// Labels for dashed percentile lines, placed in the gutter right of the
/// plot and nudged apart (by one label height) so they never overlap.
pub fn percentile_labels(
    sc: &mut Scene,
    plot: Rect,
    label_h: f32,
    labels: &mut [(f32, String, Color)],
) {
    labels.sort_by(|a, b| a.0.total_cmp(&b.0));
    let h = label_h.max(1.0);
    let half = h / 2.0;
    // Downward pass separates neighbours; upward pass pulls the stack back
    // inside the plot if it ran past the bottom.
    let mut ys: Vec<f32> = labels
        .iter()
        .map(|l| {
            l.0.clamp(plot.y0 + half, (plot.y1 - half).max(plot.y0 + half))
        })
        .collect();
    for i in 1..ys.len() {
        ys[i] = ys[i].max(ys[i - 1] + h);
    }
    if let Some(last) = ys.last_mut() {
        *last = last.min(plot.y1 - half);
    }
    for i in (0..ys.len().saturating_sub(1)).rev() {
        ys[i] = ys[i].min(ys[i + 1] - h);
    }
    for ((_, s, c), y) in labels.iter().zip(ys) {
        sc.text(
            pt(plot.x1 + palette::S8, y),
            s,
            TextSize::Small,
            *c,
            HAlign::Start,
            VAlign::Center,
        );
    }
}

/// M4 decimation: per pixel column keep the first, lowest, highest and
/// last sample, which draws identically to plotting every sample. A gap
/// breaks the line. Output goes to `pts` with segment ends in `breaks`.
pub struct Decimator<'a> {
    pts: &'a mut Vec<Point>,
    breaks: &'a mut Vec<usize>,
    col: Option<i64>,
    first: Point,
    lo: Point,
    hi: Point,
    last: Point,
    lo_at: u32,
    hi_at: u32,
    n: u32,
}

impl<'a> Decimator<'a> {
    pub fn new(pts: &'a mut Vec<Point>, breaks: &'a mut Vec<usize>) -> Decimator<'a> {
        pts.clear();
        breaks.clear();
        Decimator {
            pts,
            breaks,
            col: None,
            first: Point::default(),
            lo: Point::default(),
            hi: Point::default(),
            last: Point::default(),
            lo_at: 0,
            hi_at: 0,
            n: 0,
        }
    }

    fn flush(&mut self) {
        if self.n == 0 {
            return;
        }
        self.pts.push(self.first);
        if self.n > 2 {
            let (a, b) = if self.lo_at <= self.hi_at {
                (self.lo, self.hi)
            } else {
                (self.hi, self.lo)
            };
            if a != self.first {
                self.pts.push(a);
            }
            if b != self.last {
                self.pts.push(b);
            }
        }
        if self.n > 1 {
            self.pts.push(self.last);
        }
        self.n = 0;
    }

    pub fn push(&mut self, p: Point) {
        let col = p.x.floor() as i64;
        if self.col != Some(col) {
            self.flush();
            self.col = Some(col);
            self.first = p;
            self.lo = p;
            self.hi = p;
            self.lo_at = 0;
            self.hi_at = 0;
        }
        // Chart y grows downward.
        if p.y > self.lo.y {
            self.lo = p;
            self.lo_at = self.n;
        }
        if p.y < self.hi.y {
            self.hi = p;
            self.hi_at = self.n;
        }
        self.last = p;
        self.n += 1;
    }

    pub fn gap(&mut self) {
        self.flush();
        self.col = None;
        if self.breaks.last().copied().unwrap_or(0) != self.pts.len() {
            self.breaks.push(self.pts.len());
        }
    }

    pub fn finish(mut self) {
        self.gap();
    }
}

/// `(start, end)` index pairs of the segments a `Decimator` made.
pub fn segments(breaks: &[usize]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut start = 0;
    breaks.iter().map(move |&b| {
        let s = (start, b);
        start = b;
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_nice() {
        let mut v = Vec::new();
        nice_ticks(0.0, 100.0, 5, &mut v);
        assert_eq!(v, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        assert_eq!(nice_max(63.6), 80.0);
        assert_eq!(nice_max(95.0), 100.0);
    }

    #[test]
    fn rare_spikes_dont_set_the_axis() {
        assert_eq!(robust_top(220.0, Some(24.0), 1000), 40.0);
        // Few samples, or a maximum near the bulk: fit the maximum.
        assert_eq!(robust_top(220.0, Some(24.0), 10), 250.0);
        assert_eq!(robust_top(40.0, Some(30.0), 1000), 50.0);
    }

    #[test]
    fn view_zoom_and_pan() {
        let mut v = TimeView::default();
        assert_eq!(v.range(100.0), (0.0, 100.0));
        v.zoom(2.0, 100.0, 100.0);
        assert_eq!(v.range(100.0), (50.0, 100.0));
        assert!(v.following());
        v.pan(-20.0, 100.0);
        assert_eq!(v.range(100.0), (30.0, 80.0));
        assert!(!v.following());
        v.pan(100.0, 100.0);
        assert!(v.following());
        v.zoom(0.1, 50.0, 100.0);
        assert_eq!(v.range(100.0), (0.0, 100.0));
    }

    #[test]
    fn decimator_keeps_extremes() {
        let mut pts = Vec::new();
        let mut br = Vec::new();
        let mut d = Decimator::new(&mut pts, &mut br);
        for (i, y) in [5.0, 1.0, 9.0, 4.0].iter().enumerate() {
            d.push(pt(10.0 + i as f32 * 0.1, *y));
        }
        d.push(pt(11.5, 3.0));
        d.finish();
        let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
        assert!(ys.contains(&1.0) && ys.contains(&9.0));
        assert_eq!(*ys.first().unwrap(), 5.0);
        assert_eq!(*ys.last().unwrap(), 3.0);
        assert_eq!(br, vec![pts.len()]);
    }

    #[test]
    fn percentile_labels_never_overlap() {
        let mut sc = Scene::default();
        let plot = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        let mut labels = vec![
            (50.0, "a".to_owned(), Color::Latency),
            (51.0, "b".to_owned(), Color::Latency),
            (99.0, "c".to_owned(), Color::Latency),
        ];
        percentile_labels(&mut sc, plot, 13.0, &mut labels);
        let ys: Vec<f32> = sc
            .prims
            .iter()
            .filter_map(|p| match p {
                super::super::scene::Prim::Text { at, .. } => Some(at.y),
                _ => None,
            })
            .collect();
        for w in ys.windows(2) {
            assert!(w[1] - w[0] >= 13.0 - 1e-3, "{ys:?}");
        }
        assert!(ys.iter().all(|y| *y <= 100.0 - 6.5 + 1e-3));
    }
}
