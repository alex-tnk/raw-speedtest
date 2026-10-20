//! Draws a view-layer `Scene` with Cairo, text through PangoCairo, and the
//! hover layer (crosshair and ringed point) on top of it.

use std::f64::consts::TAU;

use gtk::prelude::*;
use gtk::{cairo, gdk, pango};

use super::style::Palette;
use crate::view::chart::Hover;
use crate::view::palette::{self, Color};
use crate::view::scene::{HAlign, Point, Prim, Scene, TextSize, VAlign};

/// Point sizes of the scene's text sizes (axis ticks and tooltips, body).
pub fn text_px(size: TextSize) -> f64 {
    match size {
        TextSize::Small => 11.0,
        TextSize::Body => 13.0,
    }
}

/// A layout in the widget's UI font at `px`, with tabular figures.
pub fn layout(widget: &impl IsA<gtk::Widget>, px: f64) -> pango::Layout {
    let layout = widget.create_pango_layout(None);
    let mut font = widget
        .pango_context()
        .font_description()
        .unwrap_or_default();
    font.set_absolute_size(px * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFontFeatures::new("tnum"));
    layout.set_attributes(Some(&attrs));
    layout
}

fn source(cr: &cairo::Context, c: gdk::RGBA, alpha: f32) {
    cr.set_source_rgba(
        f64::from(c.red()),
        f64::from(c.green()),
        f64::from(c.blue()),
        f64::from(c.alpha() * alpha),
    );
}

/// `v` moved to the nearest pixel centre (odd device widths) or edge (even),
/// so 1 pt axis-aligned lines are crisp.
fn snap(v: f32, width: f32, scale: f64) -> f64 {
    let device = (f64::from(width) * scale).round() as i64;
    let off = if device % 2 == 1 { 0.5 } else { 0.0 };
    ((f64::from(v) * scale - off).round() + off) / scale
}

fn polyline(cr: &cairo::Context, pts: &[Point], width: f32, scale: f64) {
    match pts {
        // Gridlines, ticks and percentile lines.
        [a, b] if a.x == b.x => {
            let x = snap(a.x, width, scale);
            cr.move_to(x, f64::from(a.y));
            cr.line_to(x, f64::from(b.y));
        }
        [a, b] if a.y == b.y => {
            let y = snap(a.y, width, scale);
            cr.move_to(f64::from(a.x), y);
            cr.line_to(f64::from(b.x), y);
        }
        _ => {
            for (i, p) in pts.iter().enumerate() {
                if i == 0 {
                    cr.move_to(f64::from(p.x), f64::from(p.y));
                } else {
                    cr.line_to(f64::from(p.x), f64::from(p.y));
                }
            }
        }
    }
}

fn slice<'a, T>(v: &'a [T], r: &std::ops::Range<u32>) -> &'a [T] {
    v.get(r.start as usize..r.end as usize).unwrap_or(&[])
}

/// Draws `sc` into `widget`'s Cairo context.
pub fn scene(cr: &cairo::Context, widget: &gtk::DrawingArea, sc: &Scene, pal: &Palette) {
    let scale = f64::from(widget.scale_factor().max(1));
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_cap(cairo::LineCap::Butt);
    let mut layouts: [Option<pango::Layout>; 2] = [None, None];
    for prim in &sc.prims {
        match prim {
            Prim::Line {
                range,
                color,
                alpha,
                width,
                dashed,
            } => {
                source(cr, pal.rgba(*color), *alpha);
                cr.set_line_width(f64::from(*width));
                if *dashed {
                    cr.set_dash(&palette::DASH.map(f64::from), 0.0);
                }
                polyline(cr, slice(&sc.pts, range), *width, scale);
                let _ = cr.stroke();
                if *dashed {
                    cr.set_dash(&[], 0.0);
                }
            }
            Prim::Area {
                range,
                base,
                color,
                alpha,
            } => {
                let pts = slice(&sc.pts, range);
                let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
                    continue;
                };
                source(cr, pal.rgba(*color), *alpha);
                cr.move_to(f64::from(first.x), f64::from(*base));
                for p in pts {
                    cr.line_to(f64::from(p.x), f64::from(p.y));
                }
                cr.line_to(f64::from(last.x), f64::from(*base));
                cr.close_path();
                let _ = cr.fill();
            }
            Prim::Rect {
                rect,
                color,
                alpha,
                filled,
            } => {
                source(cr, pal.rgba(*color), *alpha);
                let (x, y) = (f64::from(rect.x0), f64::from(rect.y0));
                let (w, h) = (f64::from(rect.width()), f64::from(rect.height()));
                if *filled {
                    cr.rectangle(x, y, w, h);
                    let _ = cr.fill();
                } else {
                    // A 1 pt outline inside the rectangle.
                    let half = f64::from(palette::STROKE) / 2.0;
                    cr.set_line_width(f64::from(palette::STROKE));
                    cr.rectangle(
                        x + half,
                        y + half,
                        (w - 2.0 * half).max(0.0),
                        (h - 2.0 * half).max(0.0),
                    );
                    let _ = cr.stroke();
                }
            }
            Prim::Dots {
                range,
                radius,
                color,
                alpha,
            } => {
                source(cr, pal.rgba(*color), *alpha);
                let r = f64::from(*radius);
                for p in slice(&sc.pts, range) {
                    cr.new_sub_path();
                    cr.arc(f64::from(p.x), f64::from(p.y), r, 0.0, TAU);
                }
                let _ = cr.fill();
            }
            Prim::Text {
                at,
                range,
                size,
                color,
                h,
                v,
            } => {
                let text = sc.text.get(range.start as usize..range.end as usize);
                let Some(text) = text.filter(|t| !t.is_empty()) else {
                    continue;
                };
                let layout =
                    layouts[*size as usize].get_or_insert_with(|| layout(widget, text_px(*size)));
                layout.set_text(text);
                let (w, ht) = layout.pixel_size();
                let (w, ht) = (f64::from(w), f64::from(ht));
                let x = f64::from(at.x)
                    - match h {
                        HAlign::Start => 0.0,
                        HAlign::Center => w / 2.0,
                        HAlign::End => w,
                    };
                let y = f64::from(at.y)
                    - match v {
                        VAlign::Top => 0.0,
                        VAlign::Center => ht / 2.0,
                        VAlign::Bottom => ht,
                    };
                source(cr, pal.rgba(*color), 1.0);
                cr.move_to(x, y);
                pangocairo::functions::show_layout(cr, layout);
            }
        }
    }
}

/// The crosshair across the plot and the ringed highlighted point.
pub fn hover(cr: &cairo::Context, scale: f64, sc: &Scene, h: &Hover, pal: &Palette) {
    if let Some(x) = h.crosshair {
        source(cr, pal.rgba(Color::Text3), 1.0);
        cr.set_line_width(f64::from(palette::STROKE));
        let x = snap(x, palette::STROKE, scale);
        cr.move_to(x, f64::from(sc.plot.y0));
        cr.line_to(x, f64::from(sc.plot.y1));
        let _ = cr.stroke();
    }
    if let Some((p, c)) = h.highlight {
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        cr.arc(x, y, f64::from(palette::POINT_HOVER_RADIUS), 0.0, TAU);
        source(cr, pal.rgba(c), 1.0);
        let _ = cr.fill_preserve();
        source(cr, pal.rgba(Color::Background), 1.0);
        cr.set_line_width(f64::from(palette::STROKE));
        let _ = cr.stroke();
    }
}
