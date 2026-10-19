//! Horizontal box plots: min, p25, median, p75, max, plus every point.

use std::collections::HashMap;

use eframe::egui::{Align2, Color32, Id, Pos2, Rect, Sense, Stroke, Ui, pos2};

use super::{MeshCache, Transform, key_of, nice_max, nice_ticks};
use crate::stats::Summary;
use crate::ui::text::{self, Text};
use crate::ui::theme;
use crate::ui::widgets::{Tip, tooltip};

/// Points of one row as `(value, payload index for the hover callback)`.
pub type Points<'a> = Box<dyn Fn() -> Box<dyn Iterator<Item = (f64, usize)> + 'a> + 'a>;

pub struct Row<'a> {
    pub label: String,
    pub summary: Option<Summary>,
    pub approx: bool,
    pub color: Color32,
    pub points: Points<'a>,
}

pub struct Plot<'a> {
    pub id: &'static str,
    /// Changes whenever the underlying data does.
    pub version: u64,
    pub unit: &'a str,
    pub axis_label: &'a dyn Fn(f64) -> String,
    pub value_label: &'a dyn Fn(f64) -> String,
    /// Tooltip for a hovered point (row, payload index).
    pub point_tip: &'a dyn Fn(usize, usize) -> Tip,
    pub note: &'a str,
}

/// Cached geometry per plot, so frames that only move the pointer don't
/// walk every point again.
#[derive(Default)]
pub struct Caches {
    meshes: HashMap<&'static str, MeshCache>,
    bits: Vec<u64>,
}

/// Draw rows in `rect` (label column on the left, shared axis below).
pub fn draw(
    ui: &mut Ui,
    text: &Text,
    rect: Rect,
    rows: Vec<Row<'_>>,
    plot: &Plot<'_>,
    caches: &mut Caches,
) {
    let painter = ui.painter_at(rect.expand(theme::S4));
    let area = Rect::from_min_max(
        pos2(rect.left() + theme::BOX_LABEL, rect.top()),
        pos2(rect.right() - theme::S8, rect.bottom() - theme::AXIS_HEIGHT),
    );
    let vmax = rows
        .iter()
        .filter_map(|r| r.summary.map(|s| s.max))
        .fold(0.0, f64::max);
    let v1 = nice_max(vmax);
    let tf = Transform {
        rect: area,
        t0: 0.0,
        t1: v1,
        v0: 0.0,
        v1: 1.0,
    };
    let n = rows.len().max(1);
    let row_h = (area.height() / n as f32).min(theme::BOX_ROW * 1.5);
    let half = (row_h * 0.3).min(theme::S8);
    let cy = |ri: usize| area.top() + row_h * (ri as f32 + 0.5);

    // Axis and labels, every frame.
    let mut ticks = Vec::new();
    if vmax > 0.0 {
        nice_ticks(
            0.0,
            v1,
            ((area.width() / theme::TICK_SPACING_BOX) as usize).max(2),
            &mut ticks,
        );
        let last = ticks.len().saturating_sub(1);
        for (i, &v) in ticks.iter().enumerate() {
            let x = tf.x(v).round() + 0.5;
            painter.vline(x, area.y_range(), Stroke::new(theme::STROKE, theme::BORDER));
            let align = if i == last && x > area.right() - theme::S16 {
                Align2::RIGHT_TOP
            } else {
                Align2::CENTER_TOP
            };
            text.paint(
                &painter,
                pos2(x, area.bottom() + theme::S4),
                align,
                &(plot.axis_label)(v),
                text::SMALL_2,
            );
        }
        text.paint(
            &painter,
            pos2(rect.left(), area.bottom() + theme::S4),
            Align2::LEFT_TOP,
            plot.unit,
            text::SMALL_2,
        );
    }
    for (ri, row) in rows.iter().enumerate() {
        text.paint(
            &painter,
            pos2(rect.left(), cy(ri)),
            Align2::LEFT_CENTER,
            &row.label,
            text::BODY_2,
        );
        if row.summary.is_none() {
            text.paint(
                &painter,
                pos2(area.left(), cy(ri)),
                Align2::LEFT_CENTER,
                "—",
                text::BODY_2,
            );
        }
    }

    // Boxes and points: cached tessellation.
    let key = key_of((
        plot.version,
        rect.min.x.to_bits(),
        rect.min.y.to_bits(),
        rect.max.x.to_bits(),
        rect.max.y.to_bits(),
        v1.to_bits(),
        rows.len(),
    ));
    let Caches { meshes, bits } = caches;
    let mesh = meshes.entry(plot.id).or_default();
    mesh.paint(ui, &painter, key, |b| {
        for (ri, row) in rows.iter().enumerate() {
            let Some(s) = row.summary else { continue };
            let y = cy(ri);
            let (xmin, x25, x50, x75, xmax) = (
                tf.x(s.min),
                tf.x(s.p25),
                tf.x(s.median),
                tf.x(s.p75),
                tf.x(s.max),
            );
            let c = row.color;
            let w = theme::STROKE;
            b.segment(pos2(xmin, y), pos2(x25, y), w, c);
            b.segment(pos2(x75, y), pos2(xmax, y), w, c);
            for x in [xmin, xmax] {
                b.segment(pos2(x, y - half * 0.6), pos2(x, y + half * 0.6), w, c);
            }
            let x75 = x75.max(x25 + 1.0);
            b.rect(
                Rect::from_x_y_ranges(x25..=x75, (y - half)..=(y + half)),
                theme::tint(c, theme::FILL_ALPHA),
            );
            for (a, z) in [
                (pos2(x25, y - half), pos2(x75, y - half)),
                (pos2(x25, y + half), pos2(x75, y + half)),
                (pos2(x25, y - half), pos2(x25, y + half)),
                (pos2(x75, y - half), pos2(x75, y + half)),
            ] {
                b.segment(a, z, w, c);
            }
            b.segment(
                pos2(x50, y - half),
                pos2(x50, y + half),
                theme::DATA_LINE,
                c,
            );
            // Every point, one dot per occupied pixel column.
            bits.clear();
            bits.resize(area.width() as usize / 64 + 2, 0);
            for (v, _) in (row.points)() {
                let x = tf.x(v);
                let col = (x - area.left()).max(0.0) as usize;
                let (wd, bit) = (col / 64, col % 64);
                if wd >= bits.len() || bits[wd] & (1 << bit) != 0 {
                    continue;
                }
                bits[wd] |= 1 << bit;
                b.dot(pos2(x, y), theme::POINT_RADIUS * 0.75, c);
            }
        }
    });

    // Hover: nearest point in the hovered row, else the row's box stats.
    let resp = ui.interact(rect, Id::unique(plot.id), Sense::hover());
    let Some(h) = resp.hover_pos() else { return };
    let ri = ((h.y - area.top()) / row_h).floor();
    if ri < 0.0 || ri as usize >= rows.len() {
        return;
    }
    let ri = ri as usize;
    let row = &rows[ri];
    let Some(s) = row.summary else { return };
    let y = cy(ri);
    if (h.y - y).abs() > half + 2.0 {
        return;
    }
    let mut best: Option<(f32, usize, Pos2)> = None;
    for (v, idx) in (row.points)() {
        let x = tf.x(v);
        let d = (x - h.x).abs();
        if d <= theme::HOVER_PX && best.is_none_or(|b| d < b.0) {
            best = Some((d, idx, pos2(x, y)));
        }
    }
    if let Some((_, idx, p)) = best {
        painter.circle(
            p,
            theme::POINT_HOVER_RADIUS,
            row.color,
            Stroke::new(theme::STROKE, theme::BG),
        );
        tooltip(ui, text, p, &(plot.point_tip)(ri, idx));
    } else if h.x >= tf.x(s.min) - 2.0 && h.x <= tf.x(s.max) + 2.0 {
        tooltip(
            ui,
            text,
            pos2(h.x, y - half),
            &summary_tip(&row.label, &s, row.approx, plot),
        );
    }
}

fn summary_tip(label: &str, s: &Summary, approx: bool, plot: &Plot<'_>) -> Tip {
    let v = |x: f64| format!("{} {}", (plot.value_label)(x), plot.unit);
    let mut t = Tip::default()
        .title(label.to_owned())
        .pair("Min", v(s.min))
        .pair("p25", v(s.p25))
        .pair("Median", v(s.median))
        .pair("p75", v(s.p75))
        .pair("Max", v(s.max))
        .pair("Samples", crate::ui::format::count(s.n as u64));
    if !plot.note.is_empty() {
        t = t.note(plot.note);
    }
    if approx {
        t = t.note("Percentiles from the whole-run histogram, within 0.5 %.");
    }
    t
}
