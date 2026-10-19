//! Controls and tooltips drawn to DESIGN.md: flat, 1 px borders, no
//! shadows, no animation.

use eframe::egui::{
    Align2, Color32, CornerRadius, Id, LayerId, Order, Pos2, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use super::text::{self, Style, Text};
use super::theme;

pub enum TipRow {
    Title(String),
    Pair(String, String),
    /// How a value is measured; wrapped prose.
    Note(String),
}

#[derive(Default)]
pub struct Tip {
    pub rows: Vec<TipRow>,
}

impl Tip {
    pub fn title(mut self, s: impl Into<String>) -> Tip {
        self.rows.push(TipRow::Title(s.into()));
        self
    }
    pub fn pair(mut self, k: impl Into<String>, v: impl Into<String>) -> Tip {
        self.rows.push(TipRow::Pair(k.into(), v.into()));
        self
    }
    pub fn note(mut self, s: impl Into<String>) -> Tip {
        self.rows.push(TipRow::Note(s.into()));
        self
    }
    pub fn push_pair(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.rows.push(TipRow::Pair(k.into(), v.into()));
    }
}

const TIP_PAD: f32 = theme::S8;

/// Paint a tooltip next to `anchor` without covering it.
pub fn tooltip(ui: &Ui, text: &Text, anchor: Pos2, tip: &Tip) {
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, Id::unique("raw-tip")));
    let gap = theme::S12;
    // Measure.
    let mut key_w: f32 = 0.0;
    let mut val_w: f32 = 0.0;
    let mut title_w: f32 = 0.0;
    let row_h = text.row_height(&painter, text::SMALL);
    for r in &tip.rows {
        match r {
            TipRow::Title(s) => {
                title_w = title_w.max(text.width(&painter, s, text::SMALL.semibold()))
            }
            TipRow::Pair(k, v) => {
                key_w = key_w.max(text.width(&painter, k, text::SMALL_2));
                val_w = val_w.max(text.width(&painter, v, text::SMALL));
            }
            TipRow::Note(_) => {}
        }
    }
    let pairs_w = if key_w > 0.0 {
        key_w + gap + val_w
    } else {
        0.0
    };
    let content_w = title_w
        .max(pairs_w)
        .max(theme::TIP_MIN_W)
        .min(theme::TIP_MAX_W.max(pairs_w));
    let notes: Vec<_> = tip
        .rows
        .iter()
        .filter_map(|r| match r {
            TipRow::Note(s) => Some(painter.layout(
                s.clone(),
                text::font(text::SMALL_2),
                theme::TEXT_2,
                content_w,
            )),
            _ => None,
        })
        .collect();
    let mut h = 0.0;
    for r in &tip.rows {
        h += match r {
            TipRow::Note(_) => 0.0,
            _ => row_h,
        };
    }
    for n in &notes {
        h += n.size().y + theme::S4;
    }
    let size = vec2(content_w + 2.0 * TIP_PAD, h + 2.0 * TIP_PAD);

    // Place right-above of the anchor, flipping to stay on screen.
    let screen = ui.ctx().content_rect();
    let off = theme::TOOLTIP_OFFSET;
    let mut x = anchor.x + off;
    if x + size.x > screen.right() - theme::S4 {
        x = anchor.x - off - size.x;
    }
    let mut y = anchor.y - off - size.y;
    if y < screen.top() + theme::S4 {
        y = anchor.y + off;
    }
    let rect = Rect::from_min_size(pos2(x.max(screen.left() + theme::S4), y), size);
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        theme::SURFACE,
        Stroke::new(theme::STROKE, theme::BORDER),
        StrokeKind::Inside,
    );
    let mut cy = rect.top() + TIP_PAD;
    let left = rect.left() + TIP_PAD;
    let right = rect.right() - TIP_PAD;
    let mut notes = notes.into_iter();
    for r in &tip.rows {
        match r {
            TipRow::Title(s) => {
                text.paint(
                    &painter,
                    pos2(left, cy),
                    Align2::LEFT_TOP,
                    s,
                    text::SMALL.semibold(),
                );
                cy += row_h;
            }
            TipRow::Pair(k, v) => {
                text.paint(&painter, pos2(left, cy), Align2::LEFT_TOP, k, text::SMALL_2);
                text.paint(&painter, pos2(right, cy), Align2::RIGHT_TOP, v, text::SMALL);
                cy += row_h;
            }
            TipRow::Note(_) => {
                if let Some(g) = notes.next() {
                    cy += theme::S4;
                    let hgt = g.size().y;
                    painter.galley(pos2(left, cy), g, theme::TEXT_2);
                    cy += hgt;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
}

/// A text button. Disabled buttons don't react.
pub fn button(ui: &mut Ui, text: &Text, label: &str, kind: Kind, enabled: bool) -> Response {
    let painter = ui.painter().clone();
    let w = text.width(&painter, label, text::BODY) + 2.0 * theme::BUTTON_PAD;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, theme::BUTTON_HEIGHT), sense);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    let hovered = enabled && resp.hovered();
    let (fill, stroke, color) = match (kind, enabled) {
        (_, false) => (theme::SURFACE, theme::BORDER, theme::TEXT_3),
        (Kind::Primary, true) => (theme::ACCENT, theme::ACCENT, theme::ON_ACCENT),
        (Kind::Secondary, true) => (
            if hovered {
                theme::SURFACE_RAISED
            } else {
                theme::SURFACE
            },
            if hovered {
                theme::TEXT_3
            } else {
                theme::BORDER
            },
            theme::TEXT,
        ),
    };
    let fill = if kind == Kind::Primary && hovered {
        theme::TEXT
    } else {
        fill
    };
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        fill,
        Stroke::new(theme::STROKE, stroke),
        StrokeKind::Inside,
    );
    text.paint(
        &painter,
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        text::BODY.color(color),
    );
    resp
}

/// Mutually exclusive options; returns the clicked index.
pub fn segmented(
    ui: &mut Ui,
    text: &Text,
    labels: &[&str],
    selected: usize,
    enabled: bool,
) -> Option<usize> {
    let painter = ui.painter().clone();
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| text.width(&painter, l, text::BODY) + 2.0 * theme::BUTTON_PAD)
        .collect();
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, theme::BUTTON_HEIGHT), Sense::hover());
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        theme::SURFACE,
        Stroke::new(theme::STROKE, theme::BORDER),
        StrokeKind::Inside,
    );
    let mut x = rect.left();
    let mut clicked = None;
    for (i, (label, w)) in labels.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, rect.height()));
        // The whole option set keys the id: two rows may share a label
        // ("Auto") but not their options.
        let id = ui.scope_id().with(("seg", labels, i));
        let resp = ui.interact(
            r,
            id,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let sel = i == selected;
        resp.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, sel, *label));
        if sel {
            painter.rect(
                r,
                CornerRadius::same(theme::RADIUS),
                theme::SURFACE_RAISED,
                Stroke::new(
                    theme::STROKE,
                    if enabled {
                        theme::ACCENT
                    } else {
                        theme::TEXT_3
                    },
                ),
                StrokeKind::Inside,
            );
        } else if enabled && resp.hovered() {
            painter.rect_filled(
                r.shrink(1.0),
                CornerRadius::same(theme::RADIUS),
                theme::SURFACE_RAISED,
            );
        }
        let color = match (enabled, sel) {
            (false, _) => theme::TEXT_3,
            (true, true) => theme::TEXT,
            (true, false) => theme::TEXT_2,
        };
        text.paint(
            &painter,
            r.center(),
            Align2::CENTER_CENTER,
            label,
            text::BODY.color(color),
        );
        if enabled && resp.clicked() {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
}

/// A labelled row of options for the settings panel.
pub fn option_row(
    ui: &mut Ui,
    text: &Text,
    label: &str,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    let mut out = None;
    ui.horizontal(|ui| {
        let painter = ui.painter().clone();
        let (r, _) = ui.allocate_exact_size(
            vec2(theme::SETTINGS_LABEL, theme::BUTTON_HEIGHT),
            Sense::hover(),
        );
        text.paint(
            &painter,
            pos2(r.left(), r.center().y),
            Align2::LEFT_CENTER,
            label,
            text::BODY_2,
        );
        out = segmented(ui, text, labels, selected, true);
    });
    out
}

/// Thin horizontal progress line.
pub fn progress(ui: &mut Ui, frac: Option<f32>) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::PROGRESS_HEIGHT),
        Sense::hover(),
    );
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, theme::BORDER);
    if let Some(f) = frac {
        let mut r = rect;
        r.set_width(rect.width() * f.clamp(0.0, 1.0));
        p.rect_filled(r, CornerRadius::ZERO, theme::TEXT);
    }
}

pub fn frame() -> eframe::egui::Frame {
    eframe::egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(theme::STROKE, theme::BORDER))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(theme::S12)
}

/// Label/value row for key-value tables. A value too long to share the
/// line with its label moves to a second, right-aligned line, in the small
/// size if needed, and is elided in the middle only as a last resort.
pub fn kv_row(
    ui: &mut Ui,
    text: &Text,
    label: &str,
    value: Option<&str>,
    value_style: Style,
) -> Response {
    let w = ui.available_width();
    let (v, mut style) = match value {
        Some(v) => (v.to_owned(), value_style),
        None => ("—".to_owned(), text::BODY_2),
    };
    let painter = ui.painter().clone();
    let lw = text.width(&painter, label, text::BODY_2);
    let mut v = v;
    let mut vw = text.width(&painter, &v, style);
    let two_lines = lw + theme::S12 + vw > w;
    if two_lines && vw > w {
        style = Style {
            size: theme::SMALL,
            ..style
        };
        vw = text.width(&painter, &v, style);
        if vw > w {
            let chars: Vec<char> = v.chars().collect();
            let mut keep = chars.len();
            while keep > 4 && vw > w {
                keep -= 1;
                let head: String = chars[..keep / 2].iter().collect();
                let tail: String = chars[chars.len() - (keep - keep / 2)..].iter().collect();
                v = format!("{head}…{tail}");
                vw = text.width(&painter, &v, style);
            }
        }
    }
    let h = if two_lines {
        2.0 * theme::TABLE_ROW
    } else {
        theme::TABLE_ROW
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let p = ui.painter_at(rect);
    let first = rect.top() + theme::TABLE_ROW / 2.0;
    text.paint(
        &p,
        pos2(rect.left(), first),
        Align2::LEFT_CENTER,
        label,
        text::BODY_2,
    );
    let vy = if two_lines {
        first + theme::TABLE_ROW
    } else {
        first
    };
    text.paint(&p, pos2(rect.right(), vy), Align2::RIGHT_CENTER, &v, style);
    resp
}

pub fn unavailable() -> &'static str {
    "Unavailable"
}

pub fn color_of_dir(d: crate::model::Direction) -> Color32 {
    match d {
        crate::model::Direction::Download => theme::DOWNLOAD,
        crate::model::Direction::Upload => theme::UPLOAD,
    }
}
