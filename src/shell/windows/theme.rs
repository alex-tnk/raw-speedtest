//! Light and dark appearance: the neutrals that stand in for the platform's
//! semantic colours (Win32 has none for dark mode), the accent colour, and
//! the DWM title bar attributes. Every colour and size the Windows shell
//! paints with comes from here or from `view::palette`.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWINDOWATTRIBUTE, DwmGetColorizationColor, DwmSetWindowAttribute,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::view::palette::Color;

// Neutrals, `[light, dark]`, after Windows 11's own: the window background
// (as Mica's solid fallback), text fill primary/secondary/tertiary, and
// the divider stroke. Secondary text reaches 4.5:1 on the background.
const BACKGROUND: [u32; 2] = [0xF3F3F3, 0x202020];
const TEXT: [u32; 2] = [0x1A1A1A, 0xFFFFFF];
const TEXT2: [u32; 2] = [0x5D5D5D, 0xC5C5C5];
const TEXT3: [u32; 2] = [0x8A8A8A, 0x9A9A9A];
const GRID: [u32; 2] = [0xDCDCDC, 0x3A3A3A];
/// Tooltip and popup surface.
const SURFACE: [u32; 2] = [0xFBFBFB, 0x2C2C2C];
/// Windows' default blue, when no accent colour can be read.
const ACCENT: [u32; 2] = [0x005FB8, 0x60CDFF];

// Sizes in device-independent pixels (1/96 inch).
pub const WINDOW_DEFAULT: (f32, f32) = (1440.0, 920.0);
pub const WINDOW_MIN: (f32, f32) = (1180.0, 760.0);
pub const SIDE_WIDTH: f32 = 316.0;
pub const S4: f32 = 4.0;
pub const S8: f32 = 8.0;
pub const S12: f32 = 12.0;
pub const S16: f32 = 16.0;
pub const S24: f32 = 24.0;
pub const BUTTON_HEIGHT: f32 = 28.0;
pub const BUTTON_PAD: f32 = 12.0;
pub const BUTTON_MIN: f32 = 76.0;
pub const PROGRESS_HEIGHT: f32 = 2.0;
/// A label or key/value row.
pub const ROW: f32 = 20.0;
/// Section titles.
pub const TITLE_ROW: f32 = 24.0;
pub const TAB_HEIGHT: f32 = 30.0;
/// Accent underline of the selected log tab.
pub const TAB_MARK: f32 = 2.0;
pub const LOG_HEIGHT: f32 = 300.0;
pub const THROUGHPUT_HEIGHT: f32 = 196.0;
pub const LATENCY_HEIGHT: f32 = 148.0;
pub const OVERVIEW_HEIGHT: f32 = 32.0;
pub const SWATCH: f32 = 10.0;
pub const SETTINGS_LABEL: f32 = 168.0;
pub const SETTINGS_FIELD: f32 = 160.0;
pub const TIP_MIN_W: f32 = 160.0;
pub const TIP_MAX_W: f32 = 320.0;
pub const TIP_PAD: f32 = 8.0;
/// Gap between a tooltip and the point it explains.
pub const TIP_OFFSET: f32 = 12.0;
/// Scroll distance of one wheel notch.
pub const WHEEL_LINE: f32 = 48.0;
/// Time-axis zoom per wheel notch.
pub const ZOOM_STEP: f64 = 1.25;

// Type scale (DESIGN.md), in DIPs.
pub const HERO: f32 = 56.0;
pub const SECONDARY: f32 = 32.0;
pub const SECTION: f32 = 15.0;
pub const BODY: f32 = 13.0;
pub const SMALL: f32 = 11.0;

const fn rgb(v: u32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// Accent for light and dark backgrounds.
    accent: [[u8; 3]; 2],
}

impl Theme {
    /// The user's app theme and accent colour, from the registry.
    pub fn read() -> Theme {
        let dark = reg_dword(
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
        )
        .is_some_and(|v| v == 0);
        Theme {
            dark,
            accent: accent(),
        }
    }

    fn mode(&self) -> usize {
        usize::from(self.dark)
    }

    pub fn rgb(&self, c: Color) -> [u8; 3] {
        let m = self.mode();
        if let Some(v) = c.rgb() {
            return v[m];
        }
        match c {
            Color::Text => rgb(TEXT[m]),
            Color::Text2 => rgb(TEXT2[m]),
            Color::Text3 => rgb(TEXT3[m]),
            Color::Grid => rgb(GRID[m]),
            Color::Accent => self.accent[m],
            _ => rgb(BACKGROUND[m]),
        }
    }

    pub fn surface(&self) -> [u8; 3] {
        rgb(SURFACE[self.mode()])
    }

    pub fn colorref(&self, c: Color) -> COLORREF {
        colorref(self.rgb(c))
    }

    pub fn d2d(&self, c: Color, alpha: f32) -> D2D1_COLOR_F {
        d2d(self.rgb(c), alpha)
    }
}

pub fn colorref([r, g, b]: [u8; 3]) -> COLORREF {
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

pub fn d2d([r, g, b]: [u8; 3], alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: alpha,
    }
}

/// Whether the taskbar and notification area are light (icon colour).
pub fn taskbar_light() -> bool {
    reg_dword(
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("SystemUsesLightTheme"),
    )
    .is_some_and(|v| v != 0)
}

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: `v` is a u32 and `size` says so.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut v).cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    r.is_ok().then_some(v)
}

/// The accent palette's shades meant for text and strokes on light and on
/// dark backgrounds, falling back to the DWM colourisation colour.
fn accent() -> [[u8; 3]; 2] {
    // AccentPalette: 8 RGBA entries, light 3..1, accent, dark 1..3, unused.
    let mut pal = [0u8; 32];
    let mut size = pal.len() as u32;
    // SAFETY: the buffer is `size` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(pal.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    if r.is_ok() && size >= 24 {
        let at = |i: usize| [pal[i * 4], pal[i * 4 + 1], pal[i * 4 + 2]];
        return [at(4), at(1)];
    }
    let mut argb = 0u32;
    let mut opaque = BOOL::default();
    // SAFETY: plain out-parameters.
    if unsafe { DwmGetColorizationColor(&raw mut argb, &raw mut opaque) }.is_ok() {
        let c = rgb(argb & 0xFF_FFFF);
        return [c, c];
    }
    [rgb(ACCENT[0]), rgb(ACCENT[1])]
}

fn dwm_set(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: i32) -> bool {
    // SAFETY: a 4-byte attribute value.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attr,
            (&raw const value).cast::<c_void>(),
            size_of::<i32>() as u32,
        )
    }
    .is_ok()
}

/// Title bar colours for the theme and Mica behind the title bar (the
/// client area is painted opaque). Mica needs Windows 11 22H2; older
/// systems refuse the attribute and keep a solid title bar.
pub fn apply_frame(hwnd: HWND, theme: &Theme) {
    // 20 since Windows 10 20H1; 19 on 1809–1909.
    if !dwm_set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(theme.dark)) {
        dwm_set(hwnd, DWMWINDOWATTRIBUTE(19), i32::from(theme.dark));
    }
    dwm_set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW.0);
}
