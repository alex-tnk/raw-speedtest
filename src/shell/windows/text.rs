//! GDI fonts and the owner-drawn static controls that show labels and
//! values. Statics keep their text as window text, so Narrator reads them;
//! drawing them ourselves lays every digit out in a cell as wide as the
//! widest digit (tabular figures) and puts units in a smaller, secondary
//! style on the value's baseline.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateFontIndirectW, CreateFontW,
    CreateSolidBrush, DEFAULT_CHARSET, DeleteDC, DeleteObject, EnumFontFamiliesExW, FF_SWISS,
    FW_NORMAL, FW_SEMIBOLD, FillRect, GetTextExtentPoint32W, GetTextMetricsW, HBRUSH, HDC, HFONT,
    InvalidateRect, LOGFONTW, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextAlign,
    SetTextColor, TA_BASELINE, TA_LEFT, TEXTMETRICW, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODT_STATIC, WC_STATICW};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    SetWindowLongPtrW, WINDOW_STYLE, WS_CHILD, WS_VISIBLE,
};
use windows::core::PCWSTR;

use super::theme::{self, Theme};
use super::win::{self, Scale, wide};
use crate::view::frame::MISSING;
use crate::view::palette::Color;

// Static control styles (winuser.h).
const SS_OWNERDRAW: u32 = 0x000D;
const SS_NOTIFY: u32 = 0x0100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    /// Download and upload values.
    Hero,
    /// Latency, jitter and loss values.
    Secondary,
    /// Section titles, semibold.
    Section,
    /// Units after hero values.
    Unit,
    /// Labels and values.
    Body,
    /// Small labels.
    Small,
    /// The system's message font, for buttons, combo boxes and lists.
    Ui,
}

const FONTS: usize = 7;

impl Font {
    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FontInfo {
    pub font: HFONT,
    /// Width of each digit, and the tabular cell (the widest).
    digits: [i32; 10],
    pub cell: i32,
    pub ascent: i32,
    pub height: i32,
}

/// The fonts at one DPI, and a memory DC to measure with.
pub struct Fonts {
    list: [FontInfo; FONTS],
    dc: HDC,
}

/// Whether a font family is installed.
pub fn installed(family: &str) -> bool {
    // SAFETY: a temporary memory DC.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let r = installed_in(dc, family);
        let _ = DeleteDC(dc);
        r
    }
}

fn installed_in(dc: HDC, family: &str) -> bool {
    unsafe extern "system" fn found(
        _: *const LOGFONTW,
        _: *const TEXTMETRICW,
        _: u32,
        l: LPARAM,
    ) -> i32 {
        // SAFETY: `l` points at the caller's bool for the whole enumeration.
        unsafe { *(l.0 as *mut bool) = true };
        0
    }
    let mut lf = LOGFONTW {
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    win::copy_into(family, &mut lf.lfFaceName);
    let mut hit = false;
    // SAFETY: the callback only writes `hit`, which outlives the call.
    unsafe {
        EnumFontFamiliesExW(
            dc,
            &raw const lf,
            Some(found),
            LPARAM((&raw mut hit) as isize),
            0,
        )
    };
    hit
}

impl Fonts {
    pub fn new(scale: Scale) -> Fonts {
        // SAFETY: a memory DC compatible with the screen.
        let dc = unsafe { CreateCompatibleDC(None) };
        let variable = installed_in(dc, "Segoe UI Variable Text");
        let face = |display: bool| match (variable, display) {
            (true, true) => "Segoe UI Variable Display",
            (true, false) => "Segoe UI Variable Text",
            (false, _) => "Segoe UI",
        };
        let make = |size: f32, semibold: bool, display: bool| {
            let name = wide(face(display));
            // SAFETY: plain font creation.
            unsafe {
                CreateFontW(
                    -scale.px(size),
                    0,
                    0,
                    0,
                    if semibold { FW_SEMIBOLD.0 } else { FW_NORMAL.0 } as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    FF_SWISS.0 as u32,
                    PCWSTR(name.as_ptr()),
                )
            }
        };
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        // SAFETY: `ncm` is sized for the request.
        let ui = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                ncm.cbSize,
                Some((&raw mut ncm).cast()),
                0,
                scale.dpi(),
            )
        }
        .map(|()| {
            // SAFETY: a valid LOGFONTW from the system.
            unsafe { CreateFontIndirectW(&raw const ncm.lfMessageFont) }
        })
        .unwrap_or_else(|_| make(theme::BODY, false, false));
        let handles = [
            make(theme::HERO, false, true),
            make(theme::SECONDARY, false, true),
            make(theme::SECTION, true, false),
            make(theme::SECTION, false, false),
            make(theme::BODY, false, false),
            make(theme::SMALL, false, false),
            ui,
        ];
        let list = handles.map(|font| info(dc, font));
        Fonts { list, dc }
    }

    pub fn get(&self, f: Font) -> FontInfo {
        self.list[f.index()]
    }

    pub fn all(&self) -> [FontInfo; FONTS] {
        self.list
    }

    /// Width of `s` in pixels, with tabular digits.
    pub fn measure(&self, f: Font, s: &str) -> i32 {
        let fi = self.get(f);
        let s: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: the DC and font are ours.
        unsafe { SelectObject(self.dc, fi.font.into()) };
        walk(self.dc, &fi, &s, None)
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: the fonts and DC were created by `new` and are no longer
        // selected anywhere that outlives this.
        unsafe {
            for f in &self.list {
                let _ = DeleteObject(f.font.into());
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

fn info(dc: HDC, font: HFONT) -> FontInfo {
    let mut tm = TEXTMETRICW::default();
    let mut digits = [0; 10];
    // SAFETY: measuring with our own DC.
    unsafe {
        SelectObject(dc, font.into());
        let _ = GetTextMetricsW(dc, &raw mut tm);
        for (d, w) in digits.iter_mut().enumerate() {
            *w = extent(dc, &[u16::from(b'0') + d as u16]);
        }
    }
    FontInfo {
        font,
        digits,
        cell: digits.into_iter().max().unwrap_or(0),
        ascent: tm.tmAscent,
        height: tm.tmHeight,
    }
}

fn extent(dc: HDC, s: &[u16]) -> i32 {
    let mut sz = SIZE::default();
    // SAFETY: plain measurement.
    let _ = unsafe { GetTextExtentPoint32W(dc, s, &raw mut sz) };
    sz.cx
}

fn digit(c: u16) -> Option<usize> {
    (u16::from(b'0')..=u16::from(b'9'))
        .contains(&c)
        .then(|| usize::from(c - u16::from(b'0')))
}

/// Measures `s` (the font already selected), drawing it at `at` = (x,
/// baseline) when given. Digits sit centred in tabular cells.
fn walk(dc: HDC, f: &FontInfo, s: &[u16], at: Option<(i32, i32)>) -> i32 {
    let mut w = 0;
    let mut i = 0;
    while i < s.len() {
        if let Some(d) = digit(s[i]) {
            if let Some((x, y)) = at {
                let off = (f.cell - f.digits[d]) / 2;
                // SAFETY: drawing a single glyph.
                let _ = unsafe { TextOutW(dc, x + w + off, y, &s[i..=i]) };
            }
            w += f.cell;
            i += 1;
            continue;
        }
        let j = (i..s.len())
            .find(|&k| digit(s[k]).is_some())
            .unwrap_or(s.len());
        if let Some((x, y)) = at {
            // SAFETY: drawing a run of text.
            let _ = unsafe { TextOutW(dc, x + w, y, &s[i..j]) };
        }
        w += extent(dc, &s[i..j]);
        i = j;
    }
    w
}

/// What every owner-drawn static needs at paint time, kept per thread so
/// painting never waits on the app state.
#[derive(Clone, Copy)]
pub struct Paint {
    pub theme: Theme,
    pub scale: Scale,
    pub bg: HBRUSH,
    pub fonts: [FontInfo; FONTS],
}

impl Paint {
    pub fn font(&self, f: Font) -> FontInfo {
        self.fonts[f.index()]
    }
}

thread_local! {
    static PAINT: Cell<Option<Paint>> = const { Cell::new(None) };
}

pub fn set_paint(p: Paint) {
    PAINT.with(|c| c.set(Some(p)));
}

pub fn paint() -> Option<Paint> {
    PAINT.with(Cell::get)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Swatch {
    /// A filled square (headline values).
    Square(Color),
    /// A short line sample (chart legends).
    Line(Color),
    /// A short vertical tick (loss events).
    Tick(Color),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    Label {
        font: Font,
        color: Color,
        right: bool,
        swatch: Option<Swatch>,
    },
    /// A headline value with its unit after it on the same baseline.
    Value { font: Font, unit_font: Font },
    /// Label on the left, value right-aligned; on two lines when narrow.
    Kv { two_lines: bool },
    /// A thin progress line; `frac` `None` is an empty track.
    Progress { frac: Option<f32> },
}

/// An owner-drawn static's content, owned through its `GWLP_USERDATA`.
#[derive(Clone, Debug, PartialEq)]
pub struct Static {
    pub look: Look,
    /// Text, value or label.
    pub a: String,
    /// Unit or value.
    pub b: String,
}

impl Static {
    /// What Narrator reads.
    fn accessible(&self) -> String {
        match &self.look {
            Look::Label { .. } => self.a.clone(),
            Look::Progress { frac } => {
                frac.map_or_else(String::new, |f| format!("{} %", (f * 100.0).round()))
            }
            _ if self.b.is_empty() => self.a.clone(),
            _ => format!("{} {}", self.a, self.b),
        }
    }
}

pub fn create(parent: HWND, st: Static, notify: bool) -> HWND {
    let mut style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | SS_OWNERDRAW);
    if notify {
        // Mouse messages reach the static, so its tooltip sees them.
        style.0 |= SS_NOTIFY;
    }
    let h = win::create(
        Default::default(),
        WC_STATICW,
        &st.accessible(),
        style,
        Some(parent),
        0,
        (0, 0, 0, 0),
    );
    let ptr = Box::into_raw(Box::new(st));
    // SAFETY: the box is released in `destroy`.
    unsafe { SetWindowLongPtrW(h, GWLP_USERDATA, ptr as isize) };
    h
}

fn data(h: HWND) -> *mut Static {
    // SAFETY: reading our own user data.
    unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Static }
}

pub fn destroy(h: HWND) {
    let ptr = data(h);
    // SAFETY: the window and its box were made by `create`; the box is
    // dropped after the window is gone, so painting can't reach it.
    unsafe {
        SetWindowLongPtrW(h, GWLP_USERDATA, 0);
        let _ = DestroyWindow(h);
        if !ptr.is_null() {
            drop(Box::from_raw(ptr));
        }
    }
}

/// Changes a static's content; repaints and updates its window text only
/// when something changed.
pub fn update(h: HWND, f: impl FnOnce(&mut Static)) {
    let ptr = data(h);
    if ptr.is_null() {
        return;
    }
    let text = {
        // SAFETY: UI thread only; painting reads the box only from
        // WM_DRAWITEM, never while this borrow is live.
        let st = unsafe { &mut *ptr };
        let before = st.clone();
        f(st);
        if *st == before {
            return;
        }
        (st.accessible() != before.accessible()).then(|| st.accessible())
    };
    if let Some(t) = text {
        win::set_text(h, &t);
    }
    // SAFETY: plain invalidation.
    unsafe {
        let _ = InvalidateRect(Some(h), None, false);
    }
}

pub fn get(h: HWND) -> Option<Static> {
    let ptr = data(h);
    // SAFETY: as in `update`; a short read.
    (!ptr.is_null()).then(|| unsafe { (*ptr).clone() })
}

fn fill(dc: HDC, r: &RECT, color: [u8; 3]) {
    // SAFETY: a temporary brush.
    unsafe {
        let b = CreateSolidBrush(theme::colorref(color));
        FillRect(dc, r, b);
        let _ = DeleteObject(b.into());
    }
}

/// Draws an owner-drawn static; false for other controls.
pub fn draw_item(dis: &DRAWITEMSTRUCT) -> bool {
    if dis.CtlType != ODT_STATIC {
        return false;
    }
    let ptr = data(dis.hwndItem);
    let Some(p) = paint() else {
        return false;
    };
    if ptr.is_null() {
        return false;
    }
    // SAFETY: see `update`.
    let st = unsafe { &*ptr };
    let dc = dis.hDC;
    let r = dis.rcItem;
    let t = &p.theme;
    let font = |f: Font| p.fonts[f.index()];
    // SAFETY: drawing into the DC Windows handed us for this item.
    unsafe {
        FillRect(dc, &raw const r, p.bg);
        SetBkMode(dc, TRANSPARENT);
        SetTextAlign(dc, TA_BASELINE | TA_LEFT);
    }
    let text = |s: &str, f: FontInfo, color: Color, x: i32, base: i32| {
        let s: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: as above.
        unsafe {
            SelectObject(dc, f.font.into());
            SetTextColor(dc, t.colorref(color));
        }
        walk(dc, &f, &s, Some((x, base)))
    };
    let width = |s: &str, f: FontInfo| {
        let s: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: as above.
        unsafe { SelectObject(dc, f.font.into()) };
        walk(dc, &f, &s, None)
    };
    let centred = |f: FontInfo, top: i32, h: i32| top + (h - f.height) / 2 + f.ascent;
    match &st.look {
        Look::Label {
            font: f,
            color,
            right,
            swatch,
        } => {
            let f = font(*f);
            let mut x = r.left;
            if let Some(sw) = swatch {
                let cy = (r.top + r.bottom) / 2;
                let (w, h, c) = match *sw {
                    Swatch::Square(c) => (p.scale.px(theme::SWATCH), p.scale.px(theme::SWATCH), c),
                    Swatch::Line(c) => (p.scale.px(theme::S12), p.scale.px(2.0), c),
                    Swatch::Tick(c) => (p.scale.px(2.0), p.scale.px(theme::SWATCH), c),
                };
                let sx = x + (p.scale.px(theme::S12) - w) / 2;
                let sr = RECT {
                    left: sx,
                    top: cy - h / 2,
                    right: sx + w,
                    bottom: cy - h / 2 + h,
                };
                fill(dc, &sr, t.rgb(c));
                x += p.scale.px(theme::S12 + theme::S8);
            }
            if *right {
                x = r.right - width(&st.a, f);
            }
            text(&st.a, f, *color, x, centred(f, r.top, r.bottom - r.top));
        }
        Look::Value { font: f, unit_font } => {
            let (f, u) = (font(*f), font(*unit_font));
            let base = r.top + f.ascent;
            let w = text(&st.a, f, Color::Text, r.left, base);
            if !st.b.is_empty() {
                text(
                    &st.b,
                    u,
                    Color::Text2,
                    r.left + w + p.scale.px(theme::S4),
                    base,
                );
            }
        }
        Look::Kv { two_lines } => {
            let f = font(Font::Body);
            let line = p.scale.px(theme::ROW);
            let missing = st.b == MISSING;
            let vw = width(&st.b, f);
            let vcolor = if missing { Color::Text2 } else { Color::Text };
            if *two_lines {
                text(&st.a, f, Color::Text2, r.left, centred(f, r.top, line));
                text(
                    &st.b,
                    f,
                    vcolor,
                    r.right - vw,
                    centred(f, r.top + line, line),
                );
            } else {
                let base = centred(f, r.top, r.bottom - r.top);
                text(&st.a, f, Color::Text2, r.left, base);
                text(&st.b, f, vcolor, r.right - vw, base);
            }
        }
        Look::Progress { frac } => {
            fill(dc, &r, t.rgb(Color::Grid));
            if let Some(fr) = frac {
                let w = ((r.right - r.left) as f32 * fr.clamp(0.0, 1.0)).round() as i32;
                let fr = RECT {
                    right: r.left + w,
                    ..r
                };
                fill(dc, &fr, t.rgb(Color::Accent));
            }
        }
    }
    true
}

/// Lines a key/value row needs at `width` pixels.
pub fn kv_two_lines(fonts: &Fonts, scale: Scale, label: &str, value: &str, width: i32) -> bool {
    fonts.measure(Font::Body, label) + scale.px(theme::S12) + fonts.measure(Font::Body, value)
        > width
}
