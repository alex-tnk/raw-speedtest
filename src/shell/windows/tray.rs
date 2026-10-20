//! The notification-area icon and its menu, mirroring the macOS menu bar
//! item. The icon is drawn at runtime from the system's icon font, so
//! nothing is embedded or written.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateBitmap,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_NORMAL, GdiFlush,
    OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyMenu, GetSystemMetrics, HICON,
    ICONINFO, MENU_ITEM_FLAGS, MF_CHECKED, MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    PostMessageW, RegisterWindowMessageW, SM_MENUDROPALIGNMENT, SetForegroundWindow,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_CONTEXTMENU, WM_NULL,
};
use windows::core::{BOOL, PCWSTR, w};

use super::app::{Later, WM_APP_TRAY};
use super::text;
use super::win::{copy_into, wide};
use crate::view::frame;

/// `NIN_SELECT | NINF_KEY`: the icon chosen with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

/// "SpeedHigh" in Segoe Fluent Icons and Segoe MDL2 Assets.
const GLYPH: &str = "\u{EC4A}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    RunOnce = 1,
    RunContinuously,
    PauseResume,
    Stop,
    Show,
    Quit,
}

const CMDS: [Cmd; 6] = [
    Cmd::RunOnce,
    Cmd::RunContinuously,
    Cmd::PauseResume,
    Cmd::Stop,
    Cmd::Show,
    Cmd::Quit,
];

pub struct Tray {
    hwnd: HWND,
    added: bool,
    icon: HICON,
    tip: String,
    /// Explorer broadcasts this when the taskbar is recreated.
    created_msg: u32,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        Tray {
            hwnd,
            added: false,
            icon: HICON::default(),
            tip: String::new(),
            // SAFETY: registering a well-known message name.
            created_msg: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        copy_into(&self.tip, &mut d.szTip);
        d
    }

    pub fn add(&mut self, icon: HICON, tip: &str) {
        self.icon = icon;
        self.tip = tip.to_owned();
        let mut d = self.data();
        // SAFETY: a complete NOTIFYICONDATAW for our window.
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &raw const d).as_bool() {
                d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &raw const d);
                self.added = true;
            }
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.data();
            // SAFETY: removing our own icon.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const d) };
            self.added = false;
        }
    }

    fn modify(&self) {
        if self.added {
            let d = self.data();
            // SAFETY: updating our own icon.
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) };
        }
    }

    pub fn set_tip(&mut self, tip: &str) {
        if tip != self.tip {
            self.tip = tip.to_owned();
            self.modify();
        }
    }

    pub fn set_icon(&mut self, icon: HICON, _on: bool) {
        self.icon = icon;
        self.modify();
    }

    pub fn recreated(&self, msg: u32) -> bool {
        self.created_msg != 0 && msg == self.created_msg
    }

    /// Puts the icon back after Explorer restarted.
    pub fn readd(&mut self, on: bool) {
        if on {
            self.added = false;
            let (icon, tip) = (self.icon, self.tip.clone());
            self.add(icon, &tip);
        }
    }

    /// What an icon event asks for.
    pub fn event(&self, w: WPARAM, l: LPARAM) -> Option<Later> {
        match (l.0 & 0xFFFF) as u32 {
            WM_CONTEXTMENU => {
                let (x, y) = super::win::mouse_xy(LPARAM(w.0 as isize));
                Some(Later::TrayMenu(POINT { x, y }))
            }
            NIN_SELECT | NIN_KEYSELECT => Some(Later::Show),
            _ => None,
        }
    }
}

/// Shows the icon's menu at `at` and returns the command picked. Runs a
/// modal loop: call it outside the app borrow.
pub fn menu(hwnd: HWND, t: &frame::Tray, at: POINT) -> Option<Cmd> {
    // SAFETY: a menu of our own, destroyed before returning.
    unsafe {
        let m = CreatePopupMenu().ok()?;
        let add = |flags: MENU_ITEM_FLAGS, id: usize, s: &str| {
            let s = wide(s);
            let _ = AppendMenuW(m, flags, id, PCWSTR(s.as_ptr()));
        };
        let sep = || {
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        };
        for (label, value) in &t.rows {
            add(MF_STRING | MF_DISABLED, 0, &format!("{label}\t{value}"));
        }
        sep();
        add(MF_STRING | MF_DISABLED, 0, &t.status);
        sep();
        let check = |on: bool| if on { MF_CHECKED } else { MENU_ITEM_FLAGS(0) };
        let active = t.one_shot_running || t.continuous_running;
        let enabled = |on: bool| if on { MENU_ITEM_FLAGS(0) } else { MF_GRAYED };
        add(
            MF_STRING | check(t.one_shot_running),
            Cmd::RunOnce as usize,
            "Run once",
        );
        add(
            MF_STRING | check(t.continuous_running),
            Cmd::RunContinuously as usize,
            "Run continuously",
        );
        add(
            MF_STRING | enabled(active),
            Cmd::PauseResume as usize,
            if t.paused { "Resume" } else { "Pause" },
        );
        add(MF_STRING | enabled(active), Cmd::Stop as usize, "Stop");
        sep();
        add(MF_STRING, Cmd::Show as usize, "Show window");
        add(MF_STRING, Cmd::Quit as usize, "Quit");

        // The menu closes when focus leaves it only if our window is in
        // the foreground; the null message afterwards is the documented
        // companion of this.
        let _ = SetForegroundWindow(hwnd);
        let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 {
            TPM_RIGHTALIGN
        } else {
            TPM_LEFTALIGN
        };
        let picked = TrackPopupMenu(
            m,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN | align,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(m);
        let id = picked.0 as usize;
        CMDS.into_iter().find(|c| *c as usize == id)
    }
}

/// The gauge glyph as an icon `size` pixels square, white for a dark
/// taskbar and black for a light one.
pub fn icon(size: i32, light: bool) -> Option<HICON> {
    let face = if text::installed("Segoe Fluent Icons") {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    };
    let size = size.max(16);
    // SAFETY: GDI objects of our own, all released before returning.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(
            Some(dc),
            &raw const bmi,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        ) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, color.into());
        let name = wide(face);
        let font = CreateFontW(
            -size,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            PCWSTR(name.as_ptr()),
        );
        let old_font = SelectObject(dc, font.into());
        // White on black: the grey level becomes the alpha.
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00FF_FFFF));
        SetBkMode(dc, TRANSPARENT);
        let mut g: Vec<u16> = GLYPH.encode_utf16().collect();
        let mut r = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        DrawTextW(
            dc,
            &mut g,
            &raw mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let px = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (size * size) as usize);
        let c = if light { 0 } else { 255 };
        for p in px.iter_mut() {
            let a = (*p >> 8) & 0xFF;
            // Premultiplied alpha.
            let v = c * a / 255;
            *p = (a << 24) | (v << 16) | (v << 8) | v;
        }
        SelectObject(dc, old_font);
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO {
            fIcon: BOOL::from(true),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&raw const info).ok();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        let _ = DeleteDC(dc);
        icon
    }
}
