//! The settings window: one combo box per continuous-mode setting, the
//! resulting throughput data per hour, and whether the app keeps running
//! in the notification area. Modeless and owned by the main window.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    CB_GETCURSEL, CB_SETCURSEL, CBN_SELCHANGE, DefWindowProcW, GetWindowRect, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow, WM_CLOSE,
    WM_COMMAND, WM_DPICHANGED, WM_SETFONT, WS_CAPTION, WS_EX_CONTROLPARENT, WS_POPUP, WS_SYSMENU,
};
use windows::core::{PCWSTR, w};

use super::app::{
    self, ID_SETTING0, ID_TRAY, combo, combo_field_height, combo_list, container, run_later,
    set_tip, with_app,
};
use super::text::{self, Font, Fonts, Look, Static};
use super::theme::{self, Theme};
use super::win::{self, Scale, Themed, place, send};
use crate::view::frame::{MISSING, Settings};
use crate::view::palette::Color;

pub const CLASS: PCWSTR = w!("RstSettings");

const STYLE: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE =
    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0);
/// `IDCANCEL`: Escape in dialog navigation.
const ID_CANCEL: usize = 2;

thread_local! {
    static HWND_: Cell<isize> = const { Cell::new(0) };
}

pub fn is_settings(h: HWND) -> bool {
    HWND_.with(|c| c.get() == h.0 as isize && c.get() != 0)
}

pub struct SettingsWin {
    pub hwnd: HWND,
    labels: Vec<HWND>,
    combos: Vec<HWND>,
    dph: HWND,
    tray: HWND,
    tip: HWND,
}

fn label(parent: HWND, s: &str) -> HWND {
    text::create(
        parent,
        Static {
            look: Look::Label {
                font: Font::Body,
                color: Color::Text,
                right: false,
                swatch: None,
            },
            a: s.to_owned(),
            b: String::new(),
        },
        false,
    )
}

impl SettingsWin {
    pub fn new(
        owner: HWND,
        s: &Settings,
        tray_on: bool,
        fonts: &Fonts,
        scale: Scale,
        theme: &Theme,
    ) -> SettingsWin {
        let hwnd = win::create(
            WS_EX_CONTROLPARENT,
            CLASS,
            "Settings",
            STYLE,
            Some(owner),
            0,
            (0, 0, 0, 0),
        );
        HWND_.with(|c| c.set(hwnd.0 as isize));
        let mut labels = Vec::new();
        let mut combos = Vec::new();
        for row in &s.rows {
            labels.push(label(hwnd, row.label));
            let c = combo(hwnd, &row.options, ID_SETTING0 + row.id as usize);
            send(c, CB_SETCURSEL, row.selected, 0);
            combos.push(c);
        }
        let dph = text::create(
            hwnd,
            Static {
                look: Look::Kv { two_lines: false },
                a: s.data_per_hour.label.clone(),
                b: s.data_per_hour
                    .value
                    .clone()
                    .unwrap_or_else(|| MISSING.into()),
            },
            true,
        );
        labels.push(label(hwnd, "Run in notification area"));
        let tray = combo(hwnd, &["Off".to_owned(), "On".to_owned()], ID_TRAY);
        send(tray, CB_SETCURSEL, usize::from(tray_on), 0);
        let tip = app::tooltip(hwnd, scale);
        set_tip(tip, hwnd, dph, &s.data_per_hour.tip, true);
        let mut w = SettingsWin {
            hwnd,
            labels,
            combos,
            dph,
            tray,
            tip,
        };
        w.set_fonts(fonts, scale);
        w.apply_theme(theme);
        w
    }

    fn all_combos(&self) -> impl Iterator<Item = HWND> + '_ {
        self.combos
            .iter()
            .copied()
            .chain(std::iter::once(self.tray))
    }

    pub fn set_fonts(&self, fonts: &Fonts, scale: Scale) {
        let ui = fonts.get(Font::Ui).font;
        for c in self.all_combos() {
            send(c, WM_SETFONT, ui.0 as usize, 1);
            combo_field_height(c, scale.px(theme::BUTTON_HEIGHT));
        }
        send(self.tip, WM_SETFONT, ui.0 as usize, 1);
        self.layout(scale);
    }

    fn layout(&self, s: Scale) {
        let m = s.px(theme::S16);
        let lw = s.px(theme::SETTINGS_LABEL);
        let fw = s.px(theme::SETTINGS_FIELD);
        let rh = s.px(theme::BUTTON_HEIGHT);
        let gap = s.px(theme::S8);
        let mut y = m;
        let combos: Vec<HWND> = self.all_combos().collect();
        let (last, rows) = self
            .labels
            .split_last()
            .map_or((None, &[][..]), |(l, r)| (Some(*l), r));
        for (l, c) in rows.iter().zip(&combos) {
            place(*l, m, y, lw, rh);
            place(*c, m + lw, y, fw, s.px(300.0));
            y += rh + gap;
        }
        place(self.dph, m, y, lw + fw, s.px(theme::ROW));
        y += s.px(theme::ROW) + gap * 2;
        if let Some(l) = last {
            place(l, m, y, lw, rh);
            place(self.tray, m + lw, y, fw, s.px(300.0));
            y += rh;
        }
        let mut r = RECT {
            left: 0,
            top: 0,
            right: m * 2 + lw + fw,
            bottom: y + m,
        };
        // SAFETY: sizing our own window.
        unsafe {
            let _ =
                AdjustWindowRectExForDpi(&raw mut r, STYLE, false, WS_EX_CONTROLPARENT, s.dpi());
            let mut cur = RECT::default();
            let _ = GetWindowRect(self.hwnd, &raw mut cur);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                cur.left,
                cur.top,
                win::width(&r),
                win::height(&r),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn apply_theme(&mut self, t: &Theme) {
        theme::apply_frame(self.hwnd, t);
        for c in self.all_combos().collect::<Vec<_>>() {
            win::set_theme(c, Themed::ComboBox, t.dark);
            win::set_theme(combo_list(c), Themed::Explorer, t.dark);
        }
        win::set_theme(self.tip, Themed::Explorer, t.dark);
        // SAFETY: repainting our own window.
        unsafe {
            let _ = RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME,
            );
        }
    }

    /// Centred over the main window, the first time; where it was after.
    pub fn show(&self, owner: HWND) {
        // SAFETY: positioning and showing our own windows.
        unsafe {
            if !windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.hwnd).as_bool() {
                let (mut o, mut r) = (RECT::default(), RECT::default());
                let _ = GetWindowRect(owner, &raw mut o);
                let _ = GetWindowRect(self.hwnd, &raw mut r);
                let x = o.left + (win::width(&o) - win::width(&r)) / 2;
                let y = o.top + (win::height(&o) - win::height(&r)) / 3;
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOZORDER
                        | SWP_NOACTIVATE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
                );
            }
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn update(&self, s: &Settings, prev: Option<&Settings>) {
        for (i, row) in s.rows.iter().enumerate() {
            let was = prev.and_then(|p| p.rows.get(i)).map(|r| r.selected);
            if was != Some(row.selected)
                && let Some(c) = self.combos.get(i)
            {
                send(*c, CB_SETCURSEL, row.selected, 0);
            }
        }
        let v = s
            .data_per_hour
            .value
            .clone()
            .unwrap_or_else(|| MISSING.into());
        text::update(self.dph, |st| st.b = v);
        if prev.is_none_or(|p| p.data_per_hour.tip != s.data_per_hour.tip) {
            set_tip(self.tip, self.hwnd, self.dph, &s.data_per_hour.tip, false);
        }
    }
}

pub unsafe extern "system" fn settings_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(r) = container(h, m, w, l) {
        return r;
    }
    match m {
        WM_CLOSE => {
            // SAFETY: hiding our own window.
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            let id = w.0 & 0xFFFF;
            let code = (w.0 >> 16) as u32 & 0xFFFF;
            if id == ID_CANCEL {
                // SAFETY: as above.
                unsafe {
                    let _ = ShowWindow(h, SW_HIDE);
                }
                return LRESULT(0);
            }
            if code == CBN_SELCHANGE {
                let ctl = HWND(l.0 as *mut _);
                let index = send(ctl, CB_GETCURSEL, 0, 0).0;
                if let Ok(index) = usize::try_from(index) {
                    with_app(|a| a.setting_changed(id, index));
                    run_later();
                }
                return LRESULT(0);
            }
        }
        WM_DPICHANGED => {
            // SAFETY: LPARAM is the suggested rectangle.
            let r = unsafe { *(l.0 as *const RECT) };
            // SAFETY: moving our own window.
            let _ = unsafe {
                SetWindowPos(
                    h,
                    None,
                    r.left,
                    r.top,
                    win::width(&r),
                    win::height(&r),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, m, w, l) }
}
