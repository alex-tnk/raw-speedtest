//! Small Win32 helpers: wide strings, DPI scaling, window creation.

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HMENU, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos,
    SetWindowTextW, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{PCWSTR, w};

/// NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed buffer, truncated and NUL-terminated.
pub fn copy_into(s: &str, out: &mut [u16]) {
    let Some(last) = out.len().checked_sub(1) else {
        return;
    };
    let mut n = 0;
    for (d, c) in out[..last].iter_mut().zip(s.encode_utf16()) {
        *d = c;
        n += 1;
    }
    out[n] = 0;
}

pub fn instance() -> HINSTANCE {
    // SAFETY: the module handle of this executable.
    unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(|m| HINSTANCE(m.0))
        .unwrap_or_default()
}

/// Device pixels per DIP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale(pub f32);

impl Scale {
    pub fn from_dpi(dpi: u32) -> Scale {
        Scale(dpi.max(96) as f32 / 96.0)
    }
    pub fn dpi(self) -> u32 {
        (self.0 * 96.0).round() as u32
    }
    /// DIPs to whole device pixels.
    pub fn px(self, dip: f32) -> i32 {
        (dip * self.0).round() as i32
    }
    pub fn dip(self, px: i32) -> f32 {
        px as f32 / self.0
    }
}

pub fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> LRESULT {
    // SAFETY: callers pass messages whose parameters match.
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))) }
}

#[allow(clippy::too_many_arguments)] // mirrors CreateWindowExW
pub fn create(
    ex: WINDOW_EX_STYLE,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    parent: Option<HWND>,
    id: usize,
    rect: (i32, i32, i32, i32),
) -> HWND {
    let text = wide(text);
    // SAFETY: class names are registered or system classes; the parent
    // outlives the child.
    unsafe {
        CreateWindowExW(
            ex,
            class,
            PCWSTR(text.as_ptr()),
            style,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            parent,
            (id != 0).then_some(HMENU(id as *mut _)),
            Some(instance()),
            None,
        )
    }
    .unwrap_or_default()
}

pub fn place(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    // SAFETY: plain window positioning.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            w.max(0),
            h.max(0),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

pub fn set_text(hwnd: HWND, s: &str) {
    let s = wide(s);
    // SAFETY: NUL-terminated string.
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(s.as_ptr())) };
}

/// The visual style class a control is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Themed {
    /// Buttons, scroll bars, tooltips, the list view.
    Explorer,
    ComboBox,
    Header,
}

pub fn set_theme(hwnd: HWND, kind: Themed, dark: bool) {
    // Light mode goes back to each control's default style, except the
    // list view, which takes Explorer's.
    let name = match (kind, dark) {
        (Themed::Explorer, true) => w!("DarkMode_Explorer"),
        (Themed::Explorer, false) => w!("Explorer"),
        (Themed::ComboBox, true) => w!("DarkMode_CFD"),
        (Themed::Header, true) => w!("DarkMode_ItemsView"),
        (_, false) => PCWSTR::null(),
    };
    // SAFETY: theme names are static strings.
    let _ = unsafe { SetWindowTheme(hwnd, name, PCWSTR::null()) };
}

pub fn width(r: &RECT) -> i32 {
    r.right - r.left
}

pub fn height(r: &RECT) -> i32 {
    r.bottom - r.top
}

/// Signed x and y of a mouse message's `LPARAM`.
pub fn mouse_xy(l: LPARAM) -> (i32, i32) {
    let v = l.0 as u32;
    (
        i32::from(v as u16 as i16),
        i32::from((v >> 16) as u16 as i16),
    )
}
