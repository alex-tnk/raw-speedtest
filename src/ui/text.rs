//! System UI font setup and text painting with tabular figures.
//!
//! egui shapes text without OpenType features, so `tnum` can't be turned
//! on. Instead every digit is laid out in a cell as wide as the widest
//! digit, centered, which is what tabular figures are.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::epaint::text::{
    ByteIndex, LayoutJob, LayoutSection, TextFormat, VariationCoords,
};
use eframe::egui::{
    Align2, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Galley,
    Painter, Pos2, Rect, Vec2, pos2, vec2,
};

use super::theme;
use crate::platform;

const DISPLAY: &str = "display";
const SEMIBOLD: &str = "semibold";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Semibold,
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub size: f32,
    pub weight: Weight,
    pub color: Color32,
}

impl Style {
    pub const fn new(size: f32, color: Color32) -> Style {
        Style {
            size,
            weight: Weight::Regular,
            color,
        }
    }
    pub const fn semibold(mut self) -> Style {
        self.weight = Weight::Semibold;
        self
    }
    pub const fn color(mut self, c: Color32) -> Style {
        self.color = c;
        self
    }
}

pub const BODY: Style = Style::new(theme::BODY, theme::TEXT);
pub const BODY_2: Style = Style::new(theme::BODY, theme::TEXT_2);
pub const SMALL: Style = Style::new(theme::SMALL, theme::TEXT);
pub const SMALL_2: Style = Style::new(theme::SMALL, theme::TEXT_2);
pub const SECTION: Style = Style::new(theme::SECTION, theme::TEXT).semibold();

pub fn family(size: f32, weight: Weight) -> FontFamily {
    if size >= theme::SECONDARY {
        FontFamily::Name(DISPLAY.into())
    } else {
        match weight {
            Weight::Regular => FontFamily::Proportional,
            Weight::Semibold => FontFamily::Name(SEMIBOLD.into()),
        }
    }
}

pub fn font(style: Style) -> FontId {
    FontId::new(style.size, family(style.size, style.weight))
}

/// Install the platform UI font. Returns the font's source path.
pub fn install_fonts(ctx: &Context) -> Result<String, String> {
    let f = platform::current()
        .ui_font()
        .ok_or("no system UI font found")?;
    let base = FontData::from_owned(f.data);
    let with = |weight: f32, opsz: f32| {
        let mut coords = VariationCoords::new([(b"wght", weight)]);
        if f.has_opsz {
            coords.push(b"opsz", opsz);
        }
        base.clone().tweak(FontTweak {
            coords,
            ..FontTweak::default()
        })
    };
    let mut defs = FontDefinitions::empty();
    let (regular, semibold, display) = if f.variable {
        (
            with(theme::WEIGHT_REGULAR, theme::OPSZ_TEXT),
            with(theme::WEIGHT_SEMIBOLD, theme::OPSZ_TEXT),
            with(theme::WEIGHT_REGULAR, theme::OPSZ_DISPLAY),
        )
    } else {
        let semi = f
            .semibold
            .map(FontData::from_owned)
            .unwrap_or_else(|| base.clone());
        (base.clone(), semi, base.clone())
    };
    defs.font_data.insert("regular".into(), Arc::new(regular));
    defs.font_data.insert("semibold".into(), Arc::new(semibold));
    defs.font_data.insert("display".into(), Arc::new(display));
    defs.families
        .insert(FontFamily::Proportional, vec!["regular".into()]);
    defs.families
        .insert(FontFamily::Monospace, vec!["regular".into()]);
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), vec!["semibold".into()]);
    defs.families
        .insert(FontFamily::Name(DISPLAY.into()), vec!["display".into()]);
    ctx.set_fonts(defs);
    Ok(f.source)
}

/// Digit advances per (size, family), measured once.
#[derive(Default)]
pub struct Text {
    advances: RefCell<HashMap<(u32, bool, bool), [f32; 10]>>,
}

/// A laid-out string and the width it occupies including the right pad of
/// a trailing digit.
pub struct Laid {
    pub galley: Arc<Galley>,
    pub width: f32,
}

impl Laid {
    pub fn size(&self) -> Vec2 {
        vec2(self.width, self.galley.size().y)
    }

    /// Distance from the galley top to the first row's baseline.
    pub fn baseline(&self) -> f32 {
        self.galley
            .rows
            .first()
            .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
            .unwrap_or(self.galley.size().y * 0.8)
    }
}

impl Text {
    fn digit_advances(&self, painter: &Painter, style: Style) -> [f32; 10] {
        let fam = family(style.size, style.weight);
        let key = (
            style.size.to_bits(),
            matches!(fam, FontFamily::Name(ref n) if &**n == DISPLAY),
            style.weight == Weight::Semibold,
        );
        if let Some(a) = self.advances.borrow().get(&key) {
            return *a;
        }
        let mut job = LayoutJob::default();
        let fmt = TextFormat::simple(font(style), style.color);
        for d in 0..10u8 {
            let start = job.text.len();
            job.text.push(char::from(b'0' + d));
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
                format: fmt.clone(),
            });
        }
        let g = painter.layout_job(job);
        let mut adv = [0.0f32; 10];
        if let Some(row) = g.rows.first() {
            for (i, gl) in row.row.glyphs.iter().take(10).enumerate() {
                adv[i] = gl.advance_width;
            }
        }
        self.advances.borrow_mut().insert(key, adv);
        adv
    }

    /// Lay out runs of differently styled text on one line with tabular
    /// digits.
    pub fn layout(&self, painter: &Painter, runs: &[(&str, Style)]) -> Laid {
        let mut job = LayoutJob::default();
        let mut carry = 0.0f32;
        let mut extra_width = 0.0f32;
        for (s, style) in runs {
            let adv = self.digit_advances(painter, *style);
            let cell = adv.iter().copied().fold(0.0, f32::max);
            let fmt = TextFormat::simple(font(*style), style.color);
            let mut run_start: Option<usize> = None;
            let flush = |job: &mut LayoutJob, run_start: &mut Option<usize>, carry: &mut f32| {
                if let Some(start) = run_start.take() {
                    job.sections.push(LayoutSection {
                        leading_space: *carry,
                        byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
                        format: fmt.clone(),
                    });
                    *carry = 0.0;
                }
            };
            for c in s.chars() {
                if let Some(d) = c.to_digit(10) {
                    flush(&mut job, &mut run_start, &mut carry);
                    let a = adv[d as usize];
                    let pad = (cell - a).max(0.0);
                    let left = pad / 2.0;
                    let start = job.text.len();
                    job.text.push(c);
                    job.sections.push(LayoutSection {
                        leading_space: carry + left,
                        byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
                        format: fmt.clone(),
                    });
                    carry = pad - left;
                } else {
                    if run_start.is_none() {
                        run_start = Some(job.text.len());
                    }
                    job.text.push(c);
                }
            }
            flush(&mut job, &mut run_start, &mut carry);
        }
        extra_width += carry;
        let galley = painter.layout_job(job);
        let width = galley.size().x + extra_width;
        Laid { galley, width }
    }

    pub fn laid(&self, painter: &Painter, s: &str, style: Style) -> Laid {
        self.layout(painter, &[(s, style)])
    }

    pub fn width(&self, painter: &Painter, s: &str, style: Style) -> f32 {
        self.laid(painter, s, style).width
    }

    /// Paint at `pos` anchored by `align`. Returns the painted rect.
    pub fn paint(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        s: &str,
        style: Style,
    ) -> Rect {
        let l = self.laid(painter, s, style);
        self.paint_laid(painter, pos, align, &l, style.color)
    }

    pub fn paint_laid(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        l: &Laid,
        color: Color32,
    ) -> Rect {
        let rect = align.anchor_size(pos, l.size());
        painter.galley(rect.min, l.galley.clone(), color);
        rect
    }

    /// A value followed by its unit, sharing a baseline; `pos` is the
    /// top-left of the value.
    pub fn value_unit(
        &self,
        painter: &Painter,
        pos: Pos2,
        (value, vstyle): (&str, Style),
        (unit, ustyle): (&str, Style),
    ) -> Rect {
        let gap = theme::S4;
        let v = self.laid(painter, value, vstyle);
        let u = self.laid(painter, unit, ustyle);
        let vr = Rect::from_min_size(pos, v.size());
        painter.galley(vr.min, v.galley.clone(), vstyle.color);
        if unit.is_empty() {
            return vr;
        }
        let base = pos.y + v.baseline();
        let uy = base - u.baseline();
        let ur = Rect::from_min_size(pos2(vr.max.x + gap, uy), u.size());
        painter.galley(ur.min, u.galley.clone(), ustyle.color);
        vr.union(ur)
    }

    /// `s` shortened with a trailing ellipsis to fit `max_width`.
    pub fn elide_end(&self, painter: &Painter, s: &str, style: Style, max_width: f32) -> String {
        if self.width(painter, s, style) <= max_width {
            return s.to_owned();
        }
        let chars: Vec<char> = s.chars().collect();
        let mut n = chars.len();
        while n > 0 {
            n -= 1;
            let t: String = chars[..n].iter().chain(std::iter::once(&'…')).collect();
            if self.width(painter, &t, style) <= max_width {
                return t;
            }
        }
        String::new()
    }

    pub fn row_height(&self, painter: &Painter, style: Style) -> f32 {
        self.laid(painter, "0", style).galley.size().y
    }
}
