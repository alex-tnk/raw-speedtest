//! Chart building blocks: the shared time window, axes, decimation for
//! display and cached tessellated geometry.

pub mod boxplot;
pub mod timeseries;

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use eframe::egui::epaint::{CircleShape, Mesh, PathShape, PathStroke, Tessellator};
use eframe::egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Ui, pos2};

use super::text::{self, Text};
use super::{format, theme};

/// The visible time range of the time-series charts.
#[derive(Clone, Copy, Debug, Default)]
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

/// Maps data to screen.
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
        self.rect.left() + (f as f32) * self.rect.width()
    }
    pub fn y(&self, v: f64) -> f32 {
        let f = (v - self.v0) / (self.v1 - self.v0).max(1e-12);
        self.rect.bottom() - (f as f32) * self.rect.height()
    }
    pub fn t_at(&self, x: f32) -> f64 {
        self.t0
            + f64::from((x - self.rect.left()) / self.rect.width().max(1.0)) * (self.t1 - self.t0)
    }
    pub fn pos(&self, t: f64, v: f64) -> Pos2 {
        pos2(self.x(t), self.y(v))
    }
}

/// 1-2-5 steps: ticks covering `[lo, hi]` about `target` apart.
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
pub fn time_ticks(t0: f64, t1: f64, width_px: f32, wall0: SystemTime, out: &mut Vec<f64>) -> f64 {
    out.clear();
    let max_ticks = (width_px / theme::TICK_SPACING_X).max(1.0) as f64;
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

/// Paint horizontal gridlines with value labels left of `plot`, plus an
/// optional unit label above the axis.
pub fn value_axis(
    painter: &Painter,
    text: &Text,
    plot: Rect,
    tf: &Transform,
    ticks: &[f64],
    label: &dyn Fn(f64) -> String,
    unit: &str,
) {
    for &v in ticks {
        let y = tf.y(v).round() + 0.5;
        if y < plot.top() - 0.5 || y > plot.bottom() + 0.5 {
            continue;
        }
        painter.hline(plot.x_range(), y, Stroke::new(theme::STROKE, theme::BORDER));
        text.paint(
            painter,
            pos2(plot.left() - theme::S8, y),
            Align2::RIGHT_CENTER,
            &label(v),
            text::SMALL_2,
        );
    }
    if !unit.is_empty() {
        text.paint(
            painter,
            pos2(plot.left() - theme::S8, plot.top() - theme::S8),
            Align2::RIGHT_BOTTOM,
            unit,
            text::SMALL_2,
        );
    }
}

/// Time tick labels under `plot`.
pub fn time_axis(
    painter: &Painter,
    text: &Text,
    plot: Rect,
    tf: &Transform,
    wall0: SystemTime,
    scratch: &mut Vec<f64>,
) {
    let step = time_ticks(tf.t0, tf.t1, plot.width(), wall0, scratch);
    for &t in scratch.iter() {
        let x = tf.x(t);
        painter.vline(
            x.round() + 0.5,
            plot.bottom()..=plot.bottom() + theme::S4,
            Stroke::new(theme::STROKE, theme::BORDER),
        );
        text.paint(
            painter,
            pos2(x, plot.bottom() + theme::S4),
            Align2::CENTER_TOP,
            &time_label(wall0, t, step),
            text::SMALL_2,
        );
    }
}

/// Labels for dashed percentile lines, placed in the gutter right of the
/// plot and nudged apart so they never overlap.
pub fn percentile_labels(
    painter: &Painter,
    text: &Text,
    plot: Rect,
    labels: &mut [(f32, String, Color32)],
) {
    labels.sort_by(|a, b| a.0.total_cmp(&b.0));
    let h = text.row_height(painter, text::SMALL);
    let half = h / 2.0;
    // Downward pass separates neighbours; upward pass pulls the stack back
    // inside the plot if it ran past the bottom.
    let mut ys: Vec<f32> = labels
        .iter()
        .map(|l| l.0.clamp(plot.top() + half, plot.bottom() - half))
        .collect();
    for i in 1..ys.len() {
        ys[i] = ys[i].max(ys[i - 1] + h);
    }
    if let Some(last) = ys.last_mut() {
        *last = last.min(plot.bottom() - half);
    }
    for i in (0..ys.len().saturating_sub(1)).rev() {
        ys[i] = ys[i].min(ys[i + 1] - h);
    }
    for ((_, s, c), y) in labels.iter().zip(ys) {
        text.paint(
            painter,
            pos2(plot.right() + theme::S8, y),
            Align2::LEFT_CENTER,
            s,
            text::SMALL.color(*c),
        );
    }
}

/// Tessellated chart geometry reused across frames until its key changes.
pub struct MeshCache {
    mesh: Arc<Mesh>,
    key: u64,
    tess: Option<(f32, Tessellator)>,
    path: PathShape,
}

impl Default for MeshCache {
    fn default() -> Self {
        MeshCache {
            mesh: Arc::new(Mesh::default()),
            key: u64::MAX,
            tess: None,
            path: PathShape::line(Vec::new(), PathStroke::NONE),
        }
    }
}

pub fn key_of(h: impl Hash) -> u64 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    h.hash(&mut s);
    s.finish()
}

impl MeshCache {
    /// Paint the cached mesh, rebuilding it first if `key` changed.
    pub fn paint(
        &mut self,
        ui: &Ui,
        painter: &Painter,
        key: u64,
        build: impl FnOnce(&mut Builder<'_>),
    ) {
        let ppp = ui.ctx().pixels_per_point();
        if self.key != key {
            if self.tess.as_ref().is_none_or(|(p, _)| *p != ppp) {
                let opts = ui.ctx().tessellation_options(|o| *o);
                self.tess = Some((ppp, Tessellator::new(ppp, opts, [1, 1], Vec::new())));
            }
            let mesh = Arc::make_mut(&mut self.mesh);
            mesh.clear();
            let (_, tess) = self.tess.as_mut().expect("created above");
            let mut b = Builder {
                mesh,
                tess,
                path: &mut self.path,
            };
            build(&mut b);
            self.key = key;
        }
        painter.add(eframe::egui::Shape::Mesh(self.mesh.clone()));
    }
}

pub struct Builder<'a> {
    mesh: &'a mut Mesh,
    tess: &'a mut Tessellator,
    path: &'a mut PathShape,
}

impl Builder<'_> {
    pub fn polyline(&mut self, pts: &[Pos2], width: f32, color: Color32) {
        if pts.len() < 2 {
            if let Some(p) = pts.first() {
                self.dot(*p, width * 0.75, color);
            }
            return;
        }
        self.path.points.clear();
        self.path.points.extend_from_slice(pts);
        self.path.closed = false;
        self.path.fill = Color32::TRANSPARENT;
        self.path.stroke = PathStroke::new(width, color);
        self.tess.tessellate_path(self.path, self.mesh);
    }

    /// Flat fill between a polyline and the horizontal line `base`.
    pub fn fill_under(&mut self, pts: &[Pos2], base: f32, color: Color32) {
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if (b.x - a.x).abs() < 0.01 {
                continue;
            }
            let i = self.mesh.vertices.len() as u32;
            self.mesh.colored_vertex(a, color);
            self.mesh.colored_vertex(b, color);
            self.mesh.colored_vertex(pos2(b.x, base), color);
            self.mesh.colored_vertex(pos2(a.x, base), color);
            self.mesh.add_triangle(i, i + 1, i + 2);
            self.mesh.add_triangle(i, i + 2, i + 3);
        }
    }

    pub fn rect(&mut self, r: Rect, color: Color32) {
        self.mesh.add_colored_rect(r, color);
    }

    pub fn dot(&mut self, c: Pos2, r: f32, color: Color32) {
        self.tess.tessellate_circle(
            CircleShape {
                center: c,
                radius: r,
                fill: color,
                stroke: Stroke::NONE,
            },
            self.mesh,
        );
    }

    pub fn segment(&mut self, a: Pos2, b: Pos2, width: f32, color: Color32) {
        self.tess
            .tessellate_line_segment([a, b], Stroke::new(width, color), self.mesh);
    }

    pub fn dashed_hline(&mut self, x0: f32, x1: f32, y: f32, color: Color32) {
        let mut x = x0;
        while x < x1 {
            let e = (x + theme::DASH).min(x1);
            self.segment(pos2(x, y), pos2(e, y), theme::STROKE, color);
            x = e + theme::DASH_GAP;
        }
    }
}

/// M4 decimation: per pixel column keep the first, lowest, highest and
/// last sample, which draws identically to plotting every sample. A gap
/// breaks the line. Output goes to `pts` with segment ends in `breaks`.
pub struct Decimator<'a> {
    pts: &'a mut Vec<Pos2>,
    breaks: &'a mut Vec<usize>,
    col: Option<i64>,
    first: Pos2,
    lo: Pos2,
    hi: Pos2,
    last: Pos2,
    lo_at: u32,
    hi_at: u32,
    n: u32,
}

impl<'a> Decimator<'a> {
    pub fn new(pts: &'a mut Vec<Pos2>, breaks: &'a mut Vec<usize>) -> Decimator<'a> {
        pts.clear();
        breaks.clear();
        Decimator {
            pts,
            breaks,
            col: None,
            first: Pos2::ZERO,
            lo: Pos2::ZERO,
            hi: Pos2::ZERO,
            last: Pos2::ZERO,
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

    pub fn push(&mut self, p: Pos2) {
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
        // Screen y grows downward.
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

/// Iterate `(start, end)` index pairs of the segments a `Decimator` made.
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
            d.push(pos2(10.0 + i as f32 * 0.1, *y));
        }
        d.push(pos2(11.5, 3.0));
        d.finish();
        let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
        assert!(ys.contains(&1.0) && ys.contains(&9.0));
        assert_eq!(*ys.first().unwrap(), 5.0);
        assert_eq!(*ys.last().unwrap(), 3.0);
        assert_eq!(br, vec![pts.len()]);
    }
}
