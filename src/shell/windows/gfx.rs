//! Direct2D and DirectWrite: draws chart scenes, the hover overlay and
//! tooltips. Text uses tabular figures (`tnum`) throughout.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_CAP_STYLE_FLAT, D2D1_DASH_STYLE_CUSTOM, D2D1_DASH_STYLE_SOLID,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINE_JOIN_ROUND,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_STROKE_STYLE_PROPERTIES, D2D1CreateFactory, ID2D1Factory,
    ID2D1HwndRenderTarget, ID2D1PathGeometry, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    IDWriteTextLayout, IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{BOOL, PCWSTR, Result, w};
use windows_numerics::Vector2;

use super::theme::{self, Theme};
use crate::view::chart::Hover;
use crate::view::palette::{self, Color};
use crate::view::scene::{HAlign, Point, Prim, Scene, TextSize, VAlign};
use crate::view::tip::{Tip, TipRow};

fn v(p: Point) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x0,
        top: y0,
        right: x1,
        bottom: y1,
    }
}

/// Factories and shared resources for every Direct2D surface.
pub struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    small: IDWriteTextFormat,
    body: IDWriteTextFormat,
    title: IDWriteTextFormat,
    solid: ID2D1StrokeStyle,
    dashed: ID2D1StrokeStyle,
    tnum: IDWriteTypography,
    /// Height of a small text line, in DIPs.
    pub label_h: f32,
    pts: Vec<Vector2>,
    utf16: Vec<u16>,
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        // SAFETY: COM factories; all calls on the UI thread.
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts: Option<IDWriteFontCollection> = None;
            dw.GetSystemFontCollection(&raw mut fonts, false)?;
            let family = match &fonts {
                Some(c) => {
                    let (mut i, mut found) = (0u32, BOOL::default());
                    c.FindFamilyName(w!("Segoe UI Variable Text"), &raw mut i, &raw mut found)?;
                    if found.as_bool() {
                        w!("Segoe UI Variable Text")
                    } else {
                        w!("Segoe UI")
                    }
                }
                None => w!("Segoe UI"),
            };
            let format = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
                let f = dw.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR::null(),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let small = format(theme::SMALL, DWRITE_FONT_WEIGHT_NORMAL)?;
            let body = format(theme::BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
            let title = format(theme::SMALL, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let solid = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_FLAT,
                    endCap: D2D1_CAP_STYLE_FLAT,
                    dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            // Direct2D scales custom dashes by the stroke width; dashed
            // lines are percentile rules drawn at `palette::STROKE`.
            let dashes = palette::DASH.map(|d| d / palette::STROKE);
            let dashed = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_FLAT,
                    endCap: D2D1_CAP_STYLE_FLAT,
                    dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_CUSTOM,
                    dashOffset: 0.0,
                },
                Some(&dashes),
            )?;
            let tnum = dw.CreateTypography()?;
            tnum.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            let mut g = Gfx {
                d2d,
                dw,
                small,
                body,
                title,
                solid,
                dashed,
                tnum,
                label_h: theme::SMALL * 1.33,
                pts: Vec::new(),
                utf16: Vec::new(),
            };
            if let Ok(l) = g.layout("0", TextSize::Small, f32::MAX) {
                g.label_h = metrics(&l).height.ceil();
            }
            Ok(g)
        }
    }

    pub fn target(&self, hwnd: HWND, dpi: u32, px: (u32, u32)) -> Result<Target> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: px.0.max(1),
                height: px.1.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        // SAFETY: a render target for a live window of ours.
        unsafe {
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
            let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)?;
            Ok(Target { rt, brush, px, dpi })
        }
    }

    fn format(&self, size: TextSize) -> &IDWriteTextFormat {
        match size {
            TextSize::Small => &self.small,
            TextSize::Body => &self.body,
        }
    }

    /// A single-line (or, with a finite `max_w`, wrapping) layout with
    /// tabular figures.
    fn layout(&mut self, s: &str, size: TextSize, max_w: f32) -> Result<IDWriteTextLayout> {
        let f = self.format(size).clone();
        self.layout_with(s, &f, max_w)
    }

    fn layout_with(
        &mut self,
        s: &str,
        f: &IDWriteTextFormat,
        max_w: f32,
    ) -> Result<IDWriteTextLayout> {
        self.utf16.clear();
        self.utf16.extend(s.encode_utf16());
        // SAFETY: DirectWrite calls with our own buffers.
        unsafe {
            let l = self.dw.CreateTextLayout(&self.utf16, f, max_w, f32::MAX)?;
            l.SetTypography(
                &self.tnum,
                DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: self.utf16.len() as u32,
                },
            )?;
            if max_w < f32::MAX {
                l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            }
            Ok(l)
        }
    }

    fn path(&mut self, pts: &[Point], base: Option<f32>) -> Result<ID2D1PathGeometry> {
        let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
            return Err(windows::core::Error::empty());
        };
        self.pts.clear();
        self.pts.extend(pts[1..].iter().copied().map(v));
        if let Some(b) = base {
            self.pts.push(Vector2 { X: last.x, Y: b });
            self.pts.push(Vector2 { X: first.x, Y: b });
        }
        // SAFETY: building a geometry from our own points.
        unsafe {
            let g = self.d2d.CreatePathGeometry()?;
            let sink = g.Open()?;
            sink.BeginFigure(
                v(*first),
                if base.is_some() {
                    D2D1_FIGURE_BEGIN_FILLED
                } else {
                    D2D1_FIGURE_BEGIN_HOLLOW
                },
            );
            sink.AddLines(&self.pts);
            sink.EndFigure(if base.is_some() {
                D2D1_FIGURE_END_CLOSED
            } else {
                D2D1_FIGURE_END_OPEN
            });
            sink.Close()?;
            Ok(g)
        }
    }

    /// Draws `sc` and the hover overlay; the caller has begun drawing.
    pub fn draw_scene(&mut self, t: &Target, theme: &Theme, sc: &Scene, hover: Option<&Hover>) {
        let scale = t.dpi as f32 / 96.0;
        // Thin axis-aligned rules land on pixel centres, so they stay sharp.
        let snap = |c: f32| ((c * scale).floor() + 0.5) / scale;
        let rt = &t.rt;
        let brush = &t.brush;
        for prim in &sc.prims {
            // SAFETY: drawing calls between BeginDraw and EndDraw.
            unsafe {
                match prim {
                    Prim::Line {
                        range,
                        color,
                        alpha,
                        width,
                        dashed,
                    } => {
                        let pts = &sc.pts[range.start as usize..range.end as usize];
                        brush.SetColor(&theme.d2d(*color, *alpha));
                        let style = if *dashed {
                            self.dashed.clone()
                        } else {
                            self.solid.clone()
                        };
                        if let [a, b] = pts {
                            let (mut a, mut b) = (*a, *b);
                            if *width <= 1.0 && a.y == b.y {
                                a.y = snap(a.y);
                                b.y = a.y;
                            } else if *width <= 1.0 && a.x == b.x {
                                a.x = snap(a.x);
                                b.x = a.x;
                            }
                            rt.DrawLine(v(a), v(b), brush, *width, &style);
                        } else if let Ok(g) = self.path(pts, None) {
                            rt.DrawGeometry(&g, brush, *width, &style);
                        }
                    }
                    Prim::Area {
                        range,
                        base,
                        color,
                        alpha,
                    } => {
                        let pts = &sc.pts[range.start as usize..range.end as usize];
                        if let Ok(g) = self.path(pts, Some(*base)) {
                            brush.SetColor(&theme.d2d(*color, *alpha));
                            rt.FillGeometry(&g, brush, None);
                        }
                    }
                    Prim::Rect {
                        rect: r,
                        color,
                        alpha,
                        filled,
                    } => {
                        brush.SetColor(&theme.d2d(*color, *alpha));
                        if *filled {
                            rt.FillRectangle(&rect(r.x0, r.y0, r.x1, r.y1), brush);
                        } else {
                            let h = palette::STROKE / 2.0;
                            rt.DrawRectangle(
                                &rect(r.x0 + h, r.y0 + h, r.x1 - h, r.y1 - h),
                                brush,
                                palette::STROKE,
                                None,
                            );
                        }
                    }
                    Prim::Dots {
                        range,
                        radius,
                        color,
                        alpha,
                    } => {
                        brush.SetColor(&theme.d2d(*color, *alpha));
                        for p in &sc.pts[range.start as usize..range.end as usize] {
                            rt.FillEllipse(
                                &D2D1_ELLIPSE {
                                    point: v(*p),
                                    radiusX: *radius,
                                    radiusY: *radius,
                                },
                                brush,
                            );
                        }
                    }
                    Prim::Text {
                        at,
                        range,
                        size,
                        color,
                        h,
                        v: va,
                    } => {
                        let Ok(l) = self.layout(sc.str(range), *size, f32::MAX) else {
                            continue;
                        };
                        let m = metrics(&l);
                        let x = match h {
                            HAlign::Start => at.x,
                            HAlign::Center => at.x - m.width / 2.0,
                            HAlign::End => at.x - m.width,
                        };
                        let y = match va {
                            VAlign::Top => at.y,
                            VAlign::Center => at.y - m.height / 2.0,
                            VAlign::Bottom => at.y - m.height,
                        };
                        brush.SetColor(&theme.d2d(*color, 1.0));
                        rt.DrawTextLayout(
                            Vector2 { X: x, Y: y },
                            &l,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }
        }
        let Some(hv) = hover else {
            return;
        };
        // SAFETY: as above.
        unsafe {
            if let Some(x) = hv.crosshair {
                let x = snap(x);
                brush.SetColor(&theme.d2d(Color::Text3, 1.0));
                rt.DrawLine(
                    Vector2 {
                        X: x,
                        Y: sc.plot.y0,
                    },
                    Vector2 {
                        X: x,
                        Y: sc.plot.y1,
                    },
                    brush,
                    palette::STROKE,
                    &self.solid,
                );
            }
            if let Some((p, c)) = hv.highlight {
                let e = |r: f32| D2D1_ELLIPSE {
                    point: v(p),
                    radiusX: r,
                    radiusY: r,
                };
                brush.SetColor(&theme.d2d(c, 1.0));
                rt.FillEllipse(&e(palette::POINT_HOVER_RADIUS), brush);
                brush.SetColor(&theme.d2d(Color::Background, 1.0));
                rt.DrawEllipse(
                    &e(palette::POINT_HOVER_RADIUS + palette::STROKE / 2.0),
                    brush,
                    palette::STROKE,
                    None,
                );
            }
        }
    }

    /// Lays out a tooltip; sizes are in DIPs.
    pub fn tip_layout(&mut self, tip: &Tip) -> TipLayout {
        let max = theme::TIP_MAX_W - 2.0 * theme::TIP_PAD;
        let mut lines = Vec::new();
        let mut w: f32 = theme::TIP_MIN_W - 2.0 * theme::TIP_PAD;
        let title = self.title.clone();
        for row in &tip.rows {
            match row {
                TipRow::Title(s) => {
                    if let Ok(l) = self.layout_with(s, &title, max) {
                        w = w.max(metrics(&l).width);
                        lines.push(TipLine::Title(l));
                    }
                }
                TipRow::Pair(k, val) => {
                    let (Ok(kl), Ok(vl)) = (
                        self.layout(k, TextSize::Small, f32::MAX),
                        self.layout(val, TextSize::Small, f32::MAX),
                    ) else {
                        continue;
                    };
                    w = w.max(metrics(&kl).width + theme::S16 + metrics(&vl).width);
                    lines.push(TipLine::Pair(kl, vl));
                }
                TipRow::Note(s) => {
                    if let Ok(l) = self.layout(s, TextSize::Small, max) {
                        w = w.max(metrics(&l).widthIncludingTrailingWhitespace);
                        lines.push(TipLine::Note(l));
                    }
                }
            }
        }
        let w = w.min(max).ceil();
        let mut y = theme::TIP_PAD;
        let mut placed = Vec::with_capacity(lines.len());
        for (i, line) in lines.into_iter().enumerate() {
            if i > 0 {
                y += if matches!(line, TipLine::Note(_)) {
                    theme::S4
                } else {
                    2.0
                };
            }
            let h = match &line {
                TipLine::Title(l) | TipLine::Note(l) => {
                    // SAFETY: plain layout call.
                    let _ = unsafe { l.SetMaxWidth(w) };
                    metrics(l).height
                }
                TipLine::Pair(k, val) => metrics(k).height.max(metrics(val).height),
            };
            placed.push((y, line));
            y += h;
        }
        TipLayout {
            w: w + 2.0 * theme::TIP_PAD,
            h: (y + theme::TIP_PAD).ceil(),
            lines: placed,
        }
    }

    pub fn draw_tip(&self, t: &Target, theme: &Theme, lay: &TipLayout) {
        let rt = &t.rt;
        let brush = &t.brush;
        let x0 = theme::TIP_PAD;
        let x1 = lay.w - theme::TIP_PAD;
        // SAFETY: drawing calls between BeginDraw and EndDraw.
        unsafe {
            rt.Clear(Some(&theme::d2d(theme.surface(), 1.0)));
            brush.SetColor(&theme.d2d(Color::Grid, 1.0));
            let h = palette::STROKE / 2.0;
            rt.DrawRectangle(
                &rect(h, h, lay.w - h, lay.h - h),
                brush,
                palette::STROKE,
                None,
            );
            for (y, line) in &lay.lines {
                match line {
                    TipLine::Title(l) => {
                        brush.SetColor(&theme.d2d(Color::Text, 1.0));
                        rt.DrawTextLayout(
                            Vector2 { X: x0, Y: *y },
                            l,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                    TipLine::Pair(k, val) => {
                        brush.SetColor(&theme.d2d(Color::Text2, 1.0));
                        rt.DrawTextLayout(
                            Vector2 { X: x0, Y: *y },
                            k,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        brush.SetColor(&theme.d2d(Color::Text, 1.0));
                        let vw = metrics(val).width;
                        rt.DrawTextLayout(
                            Vector2 { X: x1 - vw, Y: *y },
                            val,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                    TipLine::Note(l) => {
                        brush.SetColor(&theme.d2d(Color::Text2, 1.0));
                        rt.DrawTextLayout(
                            Vector2 { X: x0, Y: *y },
                            l,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }
        }
    }
}

fn metrics(l: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
    let mut m = DWRITE_TEXT_METRICS::default();
    // SAFETY: plain out-parameter.
    let _ = unsafe { l.GetMetrics(&raw mut m) };
    m
}

pub enum TipLine {
    Title(IDWriteTextLayout),
    Pair(IDWriteTextLayout, IDWriteTextLayout),
    Note(IDWriteTextLayout),
}

pub struct TipLayout {
    pub w: f32,
    pub h: f32,
    lines: Vec<(f32, TipLine)>,
}

/// A window's Direct2D surface.
pub struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    px: (u32, u32),
    dpi: u32,
}

impl Target {
    /// Starts a frame at the window's current pixel size and DPI.
    pub fn begin(&mut self, px: (u32, u32), dpi: u32, bg: D2D1_COLOR_F) {
        // SAFETY: render target calls on the UI thread.
        unsafe {
            if px != self.px {
                let _ = self.rt.Resize(&D2D_SIZE_U {
                    width: px.0.max(1),
                    height: px.1.max(1),
                });
                self.px = px;
            }
            if dpi != self.dpi {
                self.rt.SetDpi(dpi as f32, dpi as f32);
                self.dpi = dpi;
            }
            self.rt.BeginDraw();
            self.rt.Clear(Some(&bg));
        }
    }

    /// Ends the frame; false when the device was lost and the target must
    /// be made again.
    pub fn end(&self) -> bool {
        // SAFETY: matches `begin`.
        unsafe { self.rt.EndDraw(None, None) }.is_ok()
    }
}
