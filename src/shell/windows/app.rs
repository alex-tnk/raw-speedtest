//! The main window: toolbar, scrolling pages of values and charts, the
//! connection info column and the log. All state lives in one `App` on the
//! UI thread. Window procedures borrow it only for the length of one
//! message, and work that runs a modal loop (menus, the save dialog) or
//! must re-enter a window procedure is queued in `later` and run after the
//! borrow ends.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME,
    RDW_INVALIDATE, RedrawWindow, SetBkColor, SetTextColor,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Controls::{
    COMBOBOXINFO, DRAWITEMSTRUCT, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    ICC_TAB_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR, SetScrollInfo,
    TOOLTIPS_CLASSW, TTDT_AUTOPOP, TTDT_INITIAL, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW,
    TTM_DELTOOLW, TTM_SETDELAYTIME, TTM_SETMAXTIPWIDTH, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP,
    TTS_NOANIMATE, TTS_NOFADE, TTS_NOPREFIX, TTTOOLINFOW, WC_BUTTONW, WC_COMBOBOXW,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_DONTADDTORECENT, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileSaveDialog, IFileSaveDialog,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCOMBOBOXINFO, CB_GETCURSEL, CB_SETCURSEL, CB_SETITEMHEIGHT,
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    DefWindowProcW, DestroyWindow, DispatchMessageW, GA_ROOT, GetAncestor, GetClientRect,
    GetMessageW, GetScrollInfo, GetWindowRect, HICON, IDC_ARROW, IsDialogMessageW, IsIconic,
    IsWindowVisible, KillTimer, LoadCursorW, MINMAXINFO, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBTRACK,
    SB_TOP, SB_VERT, SCROLLINFO, SIF_ALL, SIF_DISABLENOSCROLL, SIF_PAGE, SIF_POS, SIF_RANGE,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWDEFAULT, SWP_NOACTIVATE, SWP_NOZORDER,
    SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM,
    WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_MOUSEWHEEL,
    WM_NOTIFY, WM_QUERYENDSESSION, WM_SETFONT, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER,
    WM_VSCROLL, WNDCLASSEXW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

use super::chart::{Chart, Popup};
use super::gfx::Gfx;
use super::log::LogView;
use super::settings::SettingsWin;
use super::text::{self, Font, Fonts, Look, Paint, Static, Swatch};
use super::theme::{self, Theme};
use super::tray::{self, Tray};
use super::win::{self, Scale, Themed, place, send, wide};
use crate::model::RunMode;
use crate::view::chart::boxplot;
use crate::view::chart::rssi;
use crate::view::core::{ChartId, Core};
use crate::view::frame::{self, Frame, Item, Kv, MISSING, Setting};
use crate::view::palette::Color;
use crate::view::tip::{Tip, TipRow};

const APP_NAME: &str = "Raw Speed Test";

// Control ids. Dialog navigation reserves 1 (IDOK) and 2 (IDCANCEL).
const ID_MODE: usize = 100;
const ID_START: usize = 101;
const ID_PAUSE: usize = 102;
const ID_RESET: usize = 103;
const ID_EXPORT: usize = 104;
const ID_SETTINGS: usize = 105;
pub const ID_SETTING0: usize = 200;
pub const ID_TRAY: usize = 210;

const TIMER_WAKE: usize = 1;
const TIMER_TICK: usize = 2;

/// The waker's message: `WPARAM` is the delay in milliseconds.
const WM_APP_WAKE: u32 = WM_APP + 1;
/// Notification-area icon events.
pub const WM_APP_TRAY: u32 = WM_APP + 2;

const MODES: [&str; 2] = ["One-shot", "Continuous"];

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app unless it is already borrowed further up the
/// stack (a message sent while another is handled) or not made yet.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut a = a.try_borrow_mut().ok()?;
        a.as_mut().map(f)
    })
}

static MAIN: AtomicIsize = AtomicIsize::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);

/// Work done after the app borrow ends.
pub enum Later {
    Export,
    TrayMenu(POINT),
    Show,
    Close,
}

pub struct Scroller {
    pub hwnd: HWND,
    pub page: HWND,
    pub pos: i32,
    pub content: i32,
    pub view: i32,
}

#[derive(Clone, Debug, PartialEq)]
enum Shape {
    Sub(&'static str),
    Boxes(boxplot::BoxId, usize),
    Kv(String),
}

enum ItemCtl {
    Static(HWND),
    Chart(HWND),
}

#[derive(Default)]
struct Group {
    title: HWND,
    shape: Vec<Shape>,
    items: Vec<ItemCtl>,
}

pub struct App {
    pub core: Core,
    pub hwnd: HWND,
    pub theme: Theme,
    pub scale: Scale,
    pub fonts: Fonts,
    pub gfx: Gfx,
    bg: HBRUSH,
    mode: HWND,
    start: HWND,
    pause: HWND,
    reset: HWND,
    export: HWND,
    settings_btn: HWND,
    status: HWND,
    elapsed: HWND,
    progress: HWND,
    pub main: Scroller,
    pub side: Scroller,
    /// Tooltips of the main page and the side page.
    tips: [HWND; 2],
    head: Vec<(HWND, HWND)>,
    legends: [Vec<HWND>; 2],
    pub charts: Vec<Chart>,
    columns: Vec<Group>,
    info: Vec<Group>,
    pub log: LogView,
    pub popup: Popup,
    pub tray: Tray,
    tray_on: bool,
    settings: Option<SettingsWin>,
    pub last: Option<Frame>,
    pub later: VecDeque<Later>,
    themed: Vec<(HWND, Themed)>,
    icons: [HICON; 2],
}

// ---- Entry point ----

pub fn run() {
    // SAFETY: process-wide setup on the UI thread before any window exists.
    unsafe {
        // The manifest sets per-monitor v2; this covers a missing manifest
        // and fails harmlessly otherwise.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // COM for the save dialog.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_LISTVIEW_CLASSES | ICC_TAB_CLASSES,
        };
        let _ = InitCommonControlsEx(&raw const icc);
    }
    register(w!("RstMain"), Some(main_proc), false);
    register(w!("RstScroll"), Some(scroll_proc), false);
    register(w!("RstPage"), Some(page_proc), false);
    register(super::chart::CLASS, Some(super::chart::chart_proc), true);
    register(super::chart::TIP_CLASS, Some(super::chart::tip_proc), false);
    register(
        super::settings::CLASS,
        Some(super::settings::settings_proc),
        false,
    );

    let hwnd = win::create(
        WINDOW_EX_STYLE::default(),
        w!("RstMain"),
        APP_NAME,
        WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
        None,
        0,
        (CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT),
    );
    if hwnd.is_invalid() {
        return;
    }
    MAIN.store(hwnd.0 as isize, Ordering::Release);
    let wake: crate::app::Waker = Arc::new(|delay: Duration| {
        if PENDING.swap(true, Ordering::AcqRel) {
            return;
        }
        let h = MAIN.load(Ordering::Acquire);
        let ms = delay.as_millis().min(u128::from(u32::MAX)) as usize;
        // SAFETY: posting to the main window from any thread is allowed.
        let posted =
            unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_APP_WAKE, WPARAM(ms), LPARAM(0)) };
        if posted.is_err() {
            PENDING.store(false, Ordering::Release);
        }
    });
    let Some(app) = App::new(hwnd, Core::new(wake)) else {
        return;
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|a| {
        a.size_window();
        a.refresh();
    });
    // SAFETY: showing our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
    }
    let mut msg = MSG::default();
    // SAFETY: the standard message loop.
    unsafe {
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            let root = GetAncestor(msg.hwnd, GA_ROOT);
            if !root.is_invalid()
                && is_dialog_root(root)
                && IsDialogMessageW(root, &raw const msg).as_bool()
            {
                continue;
            }
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    with_app(|a| a.tray.remove());
    // SAFETY: matches CoInitializeEx above.
    unsafe { CoUninitialize() };
}

/// Windows whose controls take Tab navigation.
fn is_dialog_root(h: HWND) -> bool {
    h.0 as isize == MAIN.load(Ordering::Acquire) || super::settings::is_settings(h)
}

fn register(name: PCWSTR, proc: WNDPROC, dblclks: bool) {
    let mut style = CS_HREDRAW | CS_VREDRAW;
    if dblclks {
        style |= CS_DBLCLKS;
    }
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style,
        lpfnWndProc: proc,
        hInstance: win::instance(),
        // SAFETY: a system cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: the class struct is complete.
    unsafe { RegisterClassExW(&raw const wc) };
}

// ---- Window procedures ----

/// Runs queued work outside the app borrow.
pub fn run_later() {
    while let Some(job) = with_app(|a| a.later.pop_front()).flatten() {
        match job {
            Later::Export => export(),
            Later::TrayMenu(at) => {
                let Some((hwnd, t)) =
                    with_app(|a| a.last.as_ref().map(|f| (a.hwnd, f.tray.clone()))).flatten()
                else {
                    continue;
                };
                if let Some(cmd) = tray::menu(hwnd, &t, at) {
                    with_app(|a| a.tray_cmd(cmd));
                }
            }
            Later::Show => {
                if let Some(h) = with_app(|a| a.hwnd) {
                    show_window(h);
                }
            }
            Later::Close => {
                if let Some(h) = with_app(|a| a.hwnd) {
                    // SAFETY: closing our own window.
                    let _ = unsafe { DestroyWindow(h) };
                }
            }
        }
    }
}

fn show_window(h: HWND) {
    // SAFETY: showing our own window.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        } else {
            let _ = ShowWindow(h, SW_SHOW);
        }
        let _ = SetForegroundWindow(h);
    }
}

/// Messages every container window answers the same way, without the app:
/// background, owner-drawn statics and control colours.
pub fn container(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    let p = text::paint()?;
    match m {
        WM_ERASEBKGND => {
            let mut r = RECT::default();
            // SAFETY: filling our own window's DC.
            unsafe {
                let _ = GetClientRect(h, &raw mut r);
                FillRect(HDC(w.0 as *mut _), &raw const r, p.bg);
            }
            Some(LRESULT(1))
        }
        WM_DRAWITEM => {
            // SAFETY: WM_DRAWITEM's LPARAM is a DRAWITEMSTRUCT.
            let dis = unsafe { &*(l.0 as *const DRAWITEMSTRUCT) };
            text::draw_item(dis).then_some(LRESULT(1))
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT
        | WM_CTLCOLORDLG => {
            let dc = HDC(w.0 as *mut _);
            // SAFETY: colouring the control's DC.
            unsafe {
                SetTextColor(dc, p.theme.colorref(Color::Text));
                SetBkColor(dc, p.theme.colorref(Color::Background));
            }
            Some(LRESULT(p.bg.0 as isize))
        }
        _ => None,
    }
}

pub fn dispatch(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    f: fn(&mut App, HWND, u32, WPARAM, LPARAM) -> Option<LRESULT>,
) -> LRESULT {
    let r = with_app(|a| f(a, h, m, w, l)).flatten();
    run_later();
    // SAFETY: default handling of a message we didn't answer.
    r.unwrap_or_else(|| unsafe { DefWindowProcW(h, m, w, l) })
}

unsafe extern "system" fn main_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            with_app(|a| a.set_dpi(u32::from(w.0 as u16)));
            // SAFETY: LPARAM is the suggested window rectangle.
            let r = unsafe { *(l.0 as *const RECT) };
            // SAFETY: resizing our own window, outside the app borrow so
            // the resulting WM_SIZE lays out at the new DPI.
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
            // The size may not have changed; the scale did.
            with_app(|a| a.layout());
            return LRESULT(0);
        }
        _ => {}
    }
    if let Some(r) = container(h, m, w, l) {
        return r;
    }
    dispatch(h, m, w, l, App::main_msg)
}

unsafe extern "system" fn scroll_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(r) = container(h, m, w, l) {
        return r;
    }
    dispatch(h, m, w, l, App::scroll_msg)
}

unsafe extern "system" fn page_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(r) = container(h, m, w, l) {
        return r;
    }
    dispatch(h, m, w, l, App::page_msg)
}

// ---- Construction ----

fn button(parent: HWND, label: &str, id: usize) -> HWND {
    win::create(
        WINDOW_EX_STYLE::default(),
        WC_BUTTONW,
        label,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
        Some(parent),
        id,
        (0, 0, 0, 0),
    )
}

pub fn combo(parent: HWND, items: &[String], id: usize) -> HWND {
    let h = win::create(
        WINDOW_EX_STYLE::default(),
        WC_COMBOBOXW,
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        Some(parent),
        id,
        (0, 0, 0, 0),
    );
    for it in items {
        let s = wide(it);
        send(h, CB_ADDSTRING, 0, s.as_ptr() as isize);
    }
    h
}

/// The drop-down list window of a combo box, to theme it.
pub fn combo_list(h: HWND) -> HWND {
    let mut info = COMBOBOXINFO {
        cbSize: size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    send(h, CB_GETCOMBOBOXINFO, 0, (&raw mut info) as isize);
    info.hwndList
}

/// A combo box sized so its closed field is `h` pixels tall.
pub fn combo_field_height(combo: HWND, h: i32) {
    // Borders add three pixels above and below the selection field.
    send(combo, CB_SETITEMHEIGHT, usize::MAX, (h - 6).max(8) as isize);
}

pub fn tooltip(owner: HWND, scale: Scale) -> HWND {
    let h = win::create(
        WS_EX_TOPMOST,
        TOOLTIPS_CLASSW,
        "",
        WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX | TTS_NOANIMATE | TTS_NOFADE),
        Some(owner),
        0,
        (0, 0, 0, 0),
    );
    send(
        h,
        TTM_SETMAXTIPWIDTH,
        0,
        scale.px(theme::TIP_MAX_W) as isize,
    );
    send(h, TTM_SETDELAYTIME, TTDT_INITIAL as usize, 0);
    send(h, TTM_SETDELAYTIME, TTDT_AUTOPOP as usize, 32_000);
    h
}

fn tool_info(owner: HWND, tool: HWND, text: &mut [u16]) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: owner,
        uId: tool.0 as usize,
        lpszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    }
}

/// Adds or updates the tooltip of `tool` (a child of `owner`).
pub fn set_tip(tt: HWND, owner: HWND, tool: HWND, tip: &Tip, add: bool) {
    let mut s = wide(&tip_text(tip));
    let ti = tool_info(owner, tool, &mut s);
    send(
        tt,
        if add {
            TTM_ADDTOOLW
        } else {
            TTM_UPDATETIPTEXTW
        },
        0,
        (&raw const ti) as isize,
    );
}

fn del_tip(tt: HWND, owner: HWND, tool: HWND) {
    let mut s = [0u16; 1];
    let ti = tool_info(owner, tool, &mut s);
    send(tt, TTM_DELTOOLW, 0, (&raw const ti) as isize);
}

/// A tooltip's rows as text: pairs one per line, notes after a blank line.
pub fn tip_text(t: &Tip) -> String {
    let mut out = String::new();
    for (i, row) in t.rows.iter().enumerate() {
        if i > 0 {
            out.push_str(if matches!(row, TipRow::Note(_)) {
                "\n\n"
            } else {
                "\n"
            });
        }
        match row {
            TipRow::Title(s) | TipRow::Note(s) => out.push_str(s),
            TipRow::Pair(k, v) => {
                out.push_str(k);
                out.push_str(": ");
                out.push_str(v);
            }
        }
    }
    out
}

fn kv_static(parent: HWND, kv: &Kv) -> HWND {
    text::create(
        parent,
        Static {
            look: Look::Kv { two_lines: false },
            a: kv.label.clone(),
            b: kv.value.clone().unwrap_or_else(|| MISSING.into()),
        },
        true,
    )
}

fn label(parent: HWND, s: &str, font: Font, color: Color, swatch: Option<Swatch>) -> HWND {
    text::create(
        parent,
        Static {
            look: Look::Label {
                font,
                color,
                right: false,
                swatch,
            },
            a: s.to_owned(),
            b: String::new(),
        },
        false,
    )
}

fn scroller(parent: HWND) -> Scroller {
    let hwnd = win::create(
        WS_EX_CONTROLPARENT,
        w!("RstScroll"),
        "",
        WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
        Some(parent),
        0,
        (0, 0, 0, 0),
    );
    let page = win::create(
        WS_EX_CONTROLPARENT,
        w!("RstPage"),
        "",
        WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
        Some(hwnd),
        0,
        (0, 0, 0, 0),
    );
    // Always show the scroll bar, so the page width never depends on the
    // content height.
    let si = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_DISABLENOSCROLL,
        ..Default::default()
    };
    // SAFETY: our own scroll bar.
    unsafe { SetScrollInfo(hwnd, SB_VERT, &raw const si, false) };
    Scroller {
        hwnd,
        page,
        pos: 0,
        content: 0,
        view: 0,
    }
}

impl App {
    fn new(hwnd: HWND, core: Core) -> Option<App> {
        let theme = Theme::read();
        // SAFETY: our own window.
        let scale = Scale::from_dpi(unsafe { GetDpiForWindow(hwnd) });
        let fonts = Fonts::new(scale);
        let gfx = Gfx::new().ok()?;
        // SAFETY: a solid brush, deleted when replaced.
        let bg = unsafe { CreateSolidBrush(theme.colorref(Color::Background)) };
        text::set_paint(Paint {
            theme,
            scale,
            bg,
            fonts: fonts.all(),
        });
        theme::apply_frame(hwnd, &theme);

        let mode = combo(hwnd, &MODES.map(String::from), ID_MODE);
        let start = button(hwnd, "Start", ID_START);
        let pause = button(hwnd, "Pause", ID_PAUSE);
        let reset = button(hwnd, "Reset", ID_RESET);
        let export = button(hwnd, "Export…", ID_EXPORT);
        let settings_btn = button(hwnd, "Settings", ID_SETTINGS);
        let status = text::create(
            hwnd,
            Static {
                look: Look::Label {
                    font: Font::Body,
                    color: Color::Text2,
                    right: false,
                    swatch: None,
                },
                a: String::new(),
                b: String::new(),
            },
            false,
        );
        let elapsed = text::create(
            hwnd,
            Static {
                look: Look::Label {
                    font: Font::Body,
                    color: Color::Text2,
                    right: true,
                    swatch: None,
                },
                a: String::new(),
                b: String::new(),
            },
            false,
        );
        let progress = text::create(
            hwnd,
            Static {
                look: Look::Progress { frac: None },
                a: String::new(),
                b: String::new(),
            },
            false,
        );
        let main = scroller(hwnd);
        let side = scroller(hwnd);
        let tips = [tooltip(hwnd, scale), tooltip(hwnd, scale)];
        let log = LogView::new(main.page, scale);
        let popup = Popup::new(hwnd);
        let tray = Tray::new(hwnd);
        let mut app = App {
            core,
            hwnd,
            theme,
            scale,
            fonts,
            gfx,
            bg,
            mode,
            start,
            pause,
            reset,
            export,
            settings_btn,
            status,
            elapsed,
            progress,
            main,
            side,
            tips,
            head: Vec::new(),
            legends: [Vec::new(), Vec::new()],
            charts: Vec::new(),
            columns: Vec::new(),
            info: Vec::new(),
            log,
            popup,
            tray,
            tray_on: false,
            settings: None,
            last: None,
            later: VecDeque::new(),
            themed: Vec::new(),
            icons: [HICON::default(); 2],
        };
        app.themed.push((mode, Themed::ComboBox));
        app.themed.push((combo_list(mode), Themed::Explorer));
        for h in [start, pause, reset, export, settings_btn] {
            app.themed.push((h, Themed::Explorer));
        }
        for h in [app.main.hwnd, app.side.hwnd, tips[0], tips[1], app.log.list] {
            app.themed.push((h, Themed::Explorer));
        }
        app.themed.push((app.log.header(), Themed::Header));
        let f = app.core.frame();
        app.build_static(&f);
        for c in [ChartId::Throughput, ChartId::Latency, ChartId::Overview] {
            let ch = Chart::new(app.main.page, c);
            app.charts.push(ch);
        }
        app.set_fonts();
        app.apply_theme(true);
        Some(app)
    }

    /// Headline and legends, which keep their shape for the whole run.
    fn build_static(&mut self, f: &Frame) {
        let page = self.main.page;
        for h in &f.headline {
            let l = text::create(
                page,
                Static {
                    look: Look::Label {
                        font: Font::Body,
                        color: Color::Text,
                        right: false,
                        swatch: Some(Swatch::Square(h.color)),
                    },
                    a: h.label.to_owned(),
                    b: String::new(),
                },
                true,
            );
            let v = text::create(
                page,
                Static {
                    look: Look::Value {
                        font: if h.hero { Font::Hero } else { Font::Secondary },
                        unit_font: if h.hero { Font::Unit } else { Font::Body },
                    },
                    a: MISSING.into(),
                    b: String::new(),
                },
                true,
            );
            // Hovering the label or the value shows how it is measured.
            set_tip(self.tips[0], page, l, &h.tip, true);
            set_tip(self.tips[0], page, v, &h.tip, true);
            self.head.push((l, v));
        }
        for (i, list) in [&f.legends.throughput, &f.legends.latency]
            .into_iter()
            .enumerate()
        {
            for lg in list {
                let sw = if lg.tick {
                    Swatch::Tick(lg.color)
                } else {
                    Swatch::Line(lg.color)
                };
                self.legends[i].push(label(page, lg.label, Font::Small, Color::Text2, Some(sw)));
            }
        }
    }

    /// Sets the control font on the system controls.
    fn set_fonts(&mut self) {
        let ui = self.fonts.get(Font::Ui).font;
        for h in [
            self.mode,
            self.start,
            self.pause,
            self.reset,
            self.export,
            self.settings_btn,
            self.tips[0],
            self.tips[1],
            self.log.tabs,
            self.log.list,
        ] {
            send(h, WM_SETFONT, ui.0 as usize, 1);
        }
        combo_field_height(self.mode, self.scale.px(theme::BUTTON_HEIGHT));
        if let Some(s) = &self.settings {
            s.set_fonts(&self.fonts, self.scale);
        }
    }

    /// Re-reads the theme; `force` applies it even when unchanged.
    fn apply_theme(&mut self, force: bool) {
        let t = Theme::read();
        if t == self.theme && !force {
            return;
        }
        self.theme = t;
        // SAFETY: swapping our brush; the old one is no longer handed out
        // once the paint state is replaced.
        unsafe {
            let old = self.bg;
            self.bg = CreateSolidBrush(t.colorref(Color::Background));
            self.update_paint();
            let _ = DeleteObject(old.into());
        }
        theme::apply_frame(self.hwnd, &t);
        for (h, kind) in &self.themed {
            win::set_theme(*h, *kind, t.dark);
        }
        self.log.apply_theme(&t);
        if let Some(s) = &mut self.settings {
            s.apply_theme(&t);
        }
        self.set_icons();
        for c in &mut self.charts {
            c.fp = 0;
        }
        self.popup.hide();
        // SAFETY: repainting our own windows.
        unsafe {
            let _ = RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME,
            );
        }
    }

    fn update_paint(&self) {
        text::set_paint(Paint {
            theme: self.theme,
            scale: self.scale,
            bg: self.bg,
            fonts: self.fonts.all(),
        });
    }

    fn set_icons(&mut self) {
        let light = theme::taskbar_light();
        let old = self.icons;
        // SAFETY: icon sizes for this window's DPI.
        let (small, big) = unsafe {
            use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
            use windows::Win32::UI::WindowsAndMessaging::{SM_CXICON, SM_CXSMICON};
            (
                GetSystemMetricsForDpi(SM_CXSMICON, self.scale.dpi()),
                GetSystemMetricsForDpi(SM_CXICON, self.scale.dpi()),
            )
        };
        self.icons = [
            tray::icon(small, light).unwrap_or_default(),
            tray::icon(big, light).unwrap_or_default(),
        ];
        send(self.hwnd, WM_SETICON, 0, self.icons[0].0 as isize);
        send(self.hwnd, WM_SETICON, 1, self.icons[1].0 as isize);
        self.tray.set_icon(self.icons[0], self.tray_on);
        for i in old {
            if !i.is_invalid() {
                // SAFETY: icons we made and replaced.
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyIcon(i) };
            }
        }
    }

    fn set_dpi(&mut self, dpi: u32) {
        let scale = Scale::from_dpi(dpi);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.fonts = Fonts::new(scale);
        self.update_paint();
        self.set_fonts();
        self.log.set_scale(scale);
        for t in self.tips {
            send(
                t,
                TTM_SETMAXTIPWIDTH,
                0,
                scale.px(theme::TIP_MAX_W) as isize,
            );
        }
        self.set_icons();
        self.popup.hide();
    }

    /// Initial window size at the window's DPI.
    fn size_window(&mut self) {
        let s = self.scale;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: s.px(theme::WINDOW_DEFAULT.0),
            bottom: s.px(theme::WINDOW_DEFAULT.1),
        };
        // SAFETY: sizing our own window.
        unsafe {
            let _ = AdjustWindowRectExForDpi(
                &raw mut r,
                WS_OVERLAPPEDWINDOW,
                false,
                WINDOW_EX_STYLE::default(),
                s.dpi(),
            );
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
        self.layout();
    }

    // ---- Messages ----

    fn main_msg(&mut self, h: HWND, m: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        match m {
            WM_SIZE => {
                self.layout();
                Some(LRESULT(0))
            }
            WM_GETMINMAXINFO => {
                let s = self.scale;
                let mut r = RECT {
                    left: 0,
                    top: 0,
                    right: s.px(theme::WINDOW_MIN.0),
                    bottom: s.px(theme::WINDOW_MIN.1),
                };
                // SAFETY: LPARAM is a MINMAXINFO; sizing at our DPI.
                unsafe {
                    let _ = AdjustWindowRectExForDpi(
                        &raw mut r,
                        WS_OVERLAPPEDWINDOW,
                        false,
                        WINDOW_EX_STYLE::default(),
                        s.dpi(),
                    );
                    let mmi = &mut *(l.0 as *mut MINMAXINFO);
                    mmi.ptMinTrackSize = POINT {
                        x: win::width(&r),
                        y: win::height(&r),
                    };
                }
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = w.0 & 0xFFFF;
                let code = (w.0 >> 16) as u32 & 0xFFFF;
                self.command(id, code);
                Some(LRESULT(0))
            }
            WM_TIMER => {
                // SAFETY: one-shot timers of ours.
                let _ = unsafe { KillTimer(Some(h), w.0) };
                if w.0 == TIMER_WAKE || w.0 == TIMER_TICK {
                    self.refresh();
                }
                Some(LRESULT(0))
            }
            WM_APP_WAKE => {
                if w.0 == 0 {
                    self.refresh();
                } else {
                    // SAFETY: a one-shot timer on our window.
                    unsafe { SetTimer(Some(h), TIMER_WAKE, w.0 as u32, None) };
                }
                Some(LRESULT(0))
            }
            WM_APP_TRAY => {
                if let Some(job) = self.tray.event(w, l) {
                    self.later.push_back(job);
                }
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                if l.0 != 0 {
                    // SAFETY: LPARAM is a NUL-terminated string when set.
                    let s = unsafe { PCWSTR(l.0 as *const u16).to_string() }.unwrap_or_default();
                    if s == "ImmersiveColorSet" {
                        self.apply_theme(false);
                        // The taskbar's theme can change on its own.
                        self.set_icons();
                    }
                }
                None
            }
            WM_DWMCOLORIZATIONCOLORCHANGED => {
                self.apply_theme(true);
                None
            }
            WM_CLOSE => {
                if self.tray_on {
                    self.popup.hide();
                    // SAFETY: hiding our own window; measuring continues.
                    unsafe {
                        let _ = ShowWindow(h, SW_HIDE);
                    }
                } else {
                    self.later.push_back(Later::Close);
                }
                Some(LRESULT(0))
            }
            WM_QUERYENDSESSION => Some(LRESULT(1)),
            WM_ENDSESSION => {
                if w.0 != 0 {
                    // The session ends: nothing to save, only the icon to
                    // take down before the process is ended.
                    self.tray.remove();
                }
                Some(LRESULT(0))
            }
            _ if self.tray.recreated(m) => {
                self.tray.readd(self.tray_on);
                Some(LRESULT(0))
            }
            WM_CTLCOLORLISTBOX => None,
            _ => None,
        }
    }

    fn scroll_msg(&mut self, h: HWND, m: u32, w: WPARAM, _l: LPARAM) -> Option<LRESULT> {
        let which = if h == self.main.hwnd {
            0
        } else if h == self.side.hwnd {
            1
        } else {
            return None;
        };
        let line = self.scale.px(theme::WHEEL_LINE);
        let sc = if which == 0 { &self.main } else { &self.side };
        let (pos, view, content) = (sc.pos, sc.view, sc.content);
        let to = match m {
            WM_VSCROLL => match (w.0 & 0xFFFF) as i32 {
                x if x == SB_LINEUP.0 => pos - line,
                x if x == SB_LINEDOWN.0 => pos + line,
                x if x == SB_PAGEUP.0 => pos - view,
                x if x == SB_PAGEDOWN.0 => pos + view,
                x if x == SB_TOP.0 => 0,
                x if x == SB_BOTTOM.0 => content,
                x if x == SB_THUMBTRACK.0 => self.thumb_pos(h),
                _ => return Some(LRESULT(0)),
            },
            WM_MOUSEWHEEL => {
                let delta = i32::from((w.0 >> 16) as u16 as i16);
                pos - delta * line / 120
            }
            _ => return None,
        };
        self.scroll_to(which, to);
        Some(LRESULT(0))
    }

    fn thumb_pos(&self, h: HWND) -> i32 {
        let mut si = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..Default::default()
        };
        // SAFETY: reading our own scroll bar.
        let _ = unsafe { GetScrollInfo(h, SB_VERT, &raw mut si) };
        si.nTrackPos
    }

    fn page_msg(&mut self, _h: HWND, m: u32, _w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        match m {
            WM_NOTIFY => {
                // SAFETY: WM_NOTIFY's LPARAM is at least an NMHDR.
                let hdr = unsafe { &*(l.0 as *const NMHDR) };
                if hdr.hwndFrom == self.log.tabs || hdr.hwndFrom == self.log.list {
                    return self.log.notify(&self.core, &self.theme, l);
                }
                None
            }
            _ => None,
        }
    }

    fn command(&mut self, id: usize, code: u32) {
        match id {
            ID_MODE if code == CBN_SELCHANGE => {
                let i = send(self.mode, CB_GETCURSEL, 0, 0).0;
                self.core.set_mode(if i == 1 {
                    RunMode::Continuous
                } else {
                    RunMode::OneShot
                });
            }
            ID_START => {
                if self.core.with_session(|s| s.active()) {
                    self.core.stop();
                } else {
                    self.core.start();
                }
            }
            ID_PAUSE => {
                if self.last.as_ref().is_some_and(|f| f.controls.paused) {
                    self.core.resume();
                } else {
                    self.core.pause();
                }
            }
            ID_RESET => self.core.reset(),
            ID_EXPORT => {
                self.later.push_back(Later::Export);
                return;
            }
            ID_SETTINGS => {
                self.open_settings();
                return;
            }
            _ => return,
        }
        self.refresh();
    }

    /// A change made in the settings window.
    pub fn setting_changed(&mut self, id: usize, index: usize) {
        if id == ID_TRAY {
            self.set_tray(index == 1);
        } else if let Some(s) = id
            .checked_sub(ID_SETTING0)
            .and_then(|i| Setting::from_u8(i as u8))
        {
            self.core.set(s, index);
        }
        self.refresh();
    }

    fn set_tray(&mut self, on: bool) {
        if on == self.tray_on {
            return;
        }
        self.tray_on = on;
        if on {
            let tip = self
                .last
                .as_ref()
                .map(|f| f.tray.title.clone())
                .unwrap_or_default();
            self.tray.add(self.icons[0], &tray_tip(&tip));
        } else {
            self.tray.remove();
            // With no icon left, a hidden window would be unreachable.
            // SAFETY: querying our own window.
            if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
                self.later.push_back(Later::Show);
            }
        }
    }

    pub fn tray_cmd(&mut self, cmd: tray::Cmd) {
        match cmd {
            tray::Cmd::RunOnce => self.core.start_mode(RunMode::OneShot),
            tray::Cmd::RunContinuously => self.core.start_mode(RunMode::Continuous),
            tray::Cmd::PauseResume => {
                if self.last.as_ref().is_some_and(|f| f.tray.paused) {
                    self.core.resume();
                } else {
                    self.core.pause();
                }
            }
            tray::Cmd::Stop => self.core.stop(),
            tray::Cmd::Show => {
                self.later.push_back(Later::Show);
                return;
            }
            tray::Cmd::Quit => {
                self.tray.remove();
                self.later.push_back(Later::Close);
                return;
            }
        }
        self.refresh();
    }

    fn open_settings(&mut self) {
        let Some(f) = &self.last else {
            return;
        };
        if self.settings.is_none() {
            let s = SettingsWin::new(
                self.hwnd,
                &f.settings,
                self.tray_on,
                &self.fonts,
                self.scale,
                &self.theme,
            );
            self.settings = Some(s);
        }
        if let Some(s) = &self.settings {
            s.show(self.hwnd);
        }
    }

    // ---- Frame ----

    /// Pulls a frame and updates whatever changed.
    pub fn refresh(&mut self) {
        PENDING.store(false, Ordering::Release);
        let f = self.core.frame();
        let relayout = self.apply(&f);
        self.last = Some(f);
        if relayout {
            self.layout();
        }
        self.update_charts();
        // SAFETY: timers on our own window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            if let Some(d) = self.core.next_tick() {
                SetTimer(
                    Some(self.hwnd),
                    TIMER_TICK,
                    d.as_millis().clamp(1, u128::from(u32::MAX)) as u32,
                    None,
                );
            }
        }
    }

    /// Updates controls whose part of the frame changed; true when the
    /// layout must be redone.
    fn apply(&mut self, f: &Frame) -> bool {
        let last = self.last.take();
        let old = last.as_ref();
        let changed = |pick: &dyn Fn(&Frame) -> bool| old.is_none_or(pick);
        let mut relayout = false;

        for (i, h) in f.headline.iter().enumerate() {
            if !changed(&|o| o.headline.get(i) != Some(h)) {
                continue;
            }
            let Some(&(l, v)) = self.head.get(i) else {
                continue;
            };
            text::update(v, |s| {
                s.a = h.value.clone().unwrap_or_else(|| MISSING.into());
                s.b = if h.value.is_some() {
                    h.unit.to_owned()
                } else {
                    String::new()
                };
            });
            if old.is_some_and(|o| o.headline.get(i).map(|x| &x.tip) != Some(&h.tip)) {
                set_tip(self.tips[0], self.main.page, l, &h.tip, false);
                set_tip(self.tips[0], self.main.page, v, &h.tip, false);
            }
        }
        if changed(&|o| o.status != f.status) {
            text::update(self.status, |s| {
                s.a = f.status.text.clone();
                if let Look::Label { color, .. } = &mut s.look {
                    *color = f.status.color;
                }
            });
        }
        if changed(&|o| o.progress != f.progress) {
            text::update(self.progress, |s| {
                s.look = Look::Progress { frac: f.progress }
            });
        }
        if changed(&|o| o.elapsed != f.elapsed) {
            text::update(self.elapsed, |s| {
                s.a = f.elapsed.clone().unwrap_or_default()
            });
        }
        if changed(&|o| o.controls != f.controls) {
            let c = &f.controls;
            send(
                self.mode,
                CB_SETCURSEL,
                usize::from(c.mode == RunMode::Continuous),
                0,
            );
            // SAFETY: enabling our own controls.
            unsafe {
                let _ = EnableWindow(self.mode, c.mode_enabled);
                let _ = EnableWindow(self.pause, c.active);
                let _ = EnableWindow(self.reset, c.can_reset);
                let _ = EnableWindow(self.export, c.can_export);
            }
            win::set_text(self.start, if c.active { "Stop" } else { "Start" });
            win::set_text(self.pause, if c.paused { "Resume" } else { "Pause" });
        }
        if let Some(s) = &self.settings
            && changed(&|o| o.settings != f.settings)
        {
            s.update(&f.settings, old.map(|o| &o.settings));
        }
        relayout |= self.apply_columns(f, old);
        relayout |= self.apply_info(f, old);
        if changed(&|o| o.rssi != f.rssi) {
            self.set_rssi(f.rssi);
            relayout = true;
        }
        if changed(&|o| o.log != f.log) {
            self.log.update(f.log);
        }
        if self.tray_on && changed(&|o| o.tray.title != f.tray.title) {
            self.tray.set_tip(&tray_tip(&f.tray.title));
        }
        relayout
    }

    fn apply_columns(&mut self, f: &Frame, old: Option<&Frame>) -> bool {
        let page = self.main.page;
        let mut relayout = false;
        if self.columns.len() != f.columns.len() {
            for g in std::mem::take(&mut self.columns) {
                self.drop_group(g, 0);
            }
            relayout = true;
        }
        for (i, col) in f.columns.iter().enumerate() {
            let shape: Vec<Shape> = col
                .items
                .iter()
                .map(|it| match it {
                    Item::Sub(s) => Shape::Sub(s),
                    Item::Boxes { id, rows } => Shape::Boxes(*id, *rows),
                    Item::Kv(kv) => Shape::Kv(kv.label.clone()),
                })
                .collect();
            if self.columns.get(i).is_none_or(|g| g.shape != shape) {
                if i < self.columns.len() {
                    let g = std::mem::take(&mut self.columns[i]);
                    self.drop_group(g, 0);
                } else {
                    self.columns.push(Group::default());
                }
                let mut g = Group {
                    title: label(page, col.title, Font::Section, Color::Text, None),
                    shape,
                    items: Vec::new(),
                };
                for it in &col.items {
                    g.items.push(match it {
                        Item::Sub(s) => {
                            ItemCtl::Static(label(page, s, Font::Small, Color::Text2, None))
                        }
                        Item::Boxes { id, .. } => {
                            let c = Chart::new(page, ChartId::Box(*id));
                            let h = c.hwnd;
                            self.charts.push(c);
                            ItemCtl::Chart(h)
                        }
                        Item::Kv(kv) => {
                            let h = kv_static(page, kv);
                            set_tip(self.tips[0], page, h, &kv.tip, true);
                            ItemCtl::Static(h)
                        }
                    });
                }
                self.columns[i] = g;
                relayout = true;
                continue;
            }
            // Same shape: update values and tips.
            let old = old.and_then(|o| o.columns.get(i));
            let statics: Vec<Option<HWND>> = self.columns[i]
                .items
                .iter()
                .map(|c| match c {
                    ItemCtl::Static(h) => Some(*h),
                    ItemCtl::Chart(_) => None,
                })
                .collect();
            for (k, (it, h)) in col.items.iter().zip(statics).enumerate() {
                if let (Item::Kv(kv), Some(h)) = (it, h) {
                    let prev = old.and_then(|o| o.items.get(k));
                    relayout |= self.update_kv(h, kv, prev, 0);
                }
            }
        }
        relayout
    }

    fn apply_info(&mut self, f: &Frame, old: Option<&Frame>) -> bool {
        let page = self.side.page;
        let shape_of = |s: &frame::Section| -> Vec<Shape> {
            std::iter::once(Shape::Sub(s.title))
                .chain(s.rows.iter().map(|r| Shape::Kv(r.label.clone())))
                .collect()
        };
        let same = self.info.len() == f.info.len()
            && self
                .info
                .iter()
                .zip(&f.info)
                .all(|(g, s)| g.shape == shape_of(s));
        if !same {
            for g in std::mem::take(&mut self.info) {
                self.drop_group(g, 1);
            }
            for s in &f.info {
                let mut g = Group {
                    title: label(page, s.title, Font::Section, Color::Text, None),
                    shape: shape_of(s),
                    items: Vec::new(),
                };
                for kv in &s.rows {
                    let h = kv_static(page, kv);
                    set_tip(self.tips[1], page, h, &kv.tip, true);
                    g.items.push(ItemCtl::Static(h));
                }
                self.info.push(g);
            }
            return true;
        }
        let mut relayout = false;
        for (gi, s) in f.info.iter().enumerate() {
            for (k, kv) in s.rows.iter().enumerate() {
                let Some(ItemCtl::Static(h)) = self.info[gi].items.get(k) else {
                    continue;
                };
                let h = *h;
                let prev = old
                    .and_then(|o| o.info.get(gi))
                    .and_then(|o| o.rows.get(k))
                    .cloned();
                relayout |= self.update_kv(h, kv, prev.map(Item::Kv).as_ref(), 1);
            }
        }
        relayout
    }

    /// Updates a key/value row; true when it now needs another line count.
    fn update_kv(&mut self, h: HWND, kv: &Kv, prev: Option<&Item>, page: usize) -> bool {
        let prev = match prev {
            Some(Item::Kv(p)) => Some(p),
            _ => None,
        };
        if prev == Some(kv) {
            return false;
        }
        let value = kv.value.clone().unwrap_or_else(|| MISSING.into());
        let before = text::get(h);
        text::update(h, |s| s.b = value.clone());
        // Every row got a tool when it was made; an empty tip shows nothing.
        if prev.is_none_or(|p| p.tip != kv.tip) {
            let owner = if page == 0 {
                self.main.page
            } else {
                self.side.page
            };
            set_tip(self.tips[page], owner, h, &kv.tip, false);
        }
        // A wider value may no longer fit beside its label.
        let Some(before) = before else {
            return false;
        };
        let mut r = RECT::default();
        // SAFETY: reading a child's size.
        let _ = unsafe { GetClientRect(h, &raw mut r) };
        let two = text::kv_two_lines(&self.fonts, self.scale, &kv.label, &value, r.right);
        matches!(before.look, Look::Kv { two_lines } if two_lines != two)
    }

    fn drop_group(&mut self, g: Group, page: usize) {
        let owner = if page == 0 {
            self.main.page
        } else {
            self.side.page
        };
        text::destroy(g.title);
        for it in g.items {
            match it {
                ItemCtl::Static(h) => {
                    del_tip(self.tips[page], owner, h);
                    text::destroy(h);
                }
                ItemCtl::Chart(h) => self.drop_chart(h),
            }
        }
    }

    fn drop_chart(&mut self, h: HWND) {
        if let Some(i) = self.charts.iter().position(|c| c.hwnd == h) {
            let c = self.charts.swap_remove(i);
            if self.popup.owner == Some(h) {
                self.popup.hide();
            }
            drop(c);
            // SAFETY: a child window of ours.
            let _ = unsafe { DestroyWindow(h) };
        }
    }

    fn set_rssi(&mut self, on: bool) {
        let have = self
            .charts
            .iter()
            .find(|c| c.id == ChartId::Rssi)
            .map(|c| c.hwnd);
        match (on, have) {
            (true, None) => {
                let c = Chart::new(self.side.page, ChartId::Rssi);
                self.charts.push(c);
            }
            (false, Some(h)) => self.drop_chart(h),
            _ => {}
        }
    }

    // ---- Layout ----

    pub fn layout(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: our own window.
        let _ = unsafe { GetClientRect(self.hwnd, &raw mut rc) };
        let (w, h) = (rc.right, rc.bottom);
        if w <= 0 || h <= 0 {
            return;
        }
        let s = self.scale;
        let m = s.px(theme::S16);
        let top = s.px(theme::S12);
        let bh = s.px(theme::BUTTON_HEIGHT);
        let gap = s.px(theme::S8);
        let mut x = m;
        let cw = MODES
            .iter()
            .map(|t| self.fonts.measure(Font::Ui, t))
            .max()
            .unwrap_or(0)
            + s.px(theme::S24 + theme::S16);
        place(self.mode, x, top, cw, s.px(300.0));
        x += cw + gap;
        let buttons: [(HWND, &[&str]); 5] = [
            (self.start, &["Start", "Stop"]),
            (self.pause, &["Pause", "Resume"]),
            (self.reset, &["Reset"]),
            (self.export, &["Export…"]),
            (self.settings_btn, &["Settings"]),
        ];
        for (b, labels) in buttons {
            let tw = labels
                .iter()
                .map(|t| self.fonts.measure(Font::Ui, t))
                .max()
                .unwrap_or(0);
            let bw = (tw + 2 * s.px(theme::BUTTON_PAD)).max(s.px(theme::BUTTON_MIN));
            place(b, x, top, bw, bh);
            x += bw + gap;
        }
        x += gap;
        let ew = ["00d 00:00", "00:00:00"]
            .iter()
            .map(|t| self.fonts.measure(Font::Body, t))
            .max()
            .unwrap_or(0)
            + gap;
        place(self.elapsed, w - m - ew, top, ew, bh);
        place(self.status, x, top, (w - m - ew - gap - x).max(0), bh);
        let py = top + bh + gap;
        let ph = s.px(theme::PROGRESS_HEIGHT).max(1);
        place(self.progress, m, py, w - 2 * m, ph);
        let tb = py + ph + gap;

        let side_w = s.px(theme::SIDE_WIDTH);
        place(self.main.hwnd, 0, tb, w - side_w, h - tb);
        place(self.side.hwnd, w - side_w, tb, side_w, h - tb);
        let pw = client_width(self.main.hwnd);
        let content = self.layout_main(pw);
        self.set_page(0, pw, content);
        let pw = client_width(self.side.hwnd);
        let content = self.layout_side(pw);
        self.set_page(1, pw, content);
        self.update_charts();
    }

    fn place_chart(&mut self, id: ChartId, x: i32, y: i32, w: i32, h: i32) {
        let s = self.scale;
        if let Some(c) = self.charts.iter_mut().find(|c| c.id == id) {
            c.set_rect(s, x, y, w, h);
        }
    }

    fn layout_main(&mut self, pw: i32) -> i32 {
        let s = self.scale;
        let x0 = s.px(theme::S16);
        let cw = (pw - x0 - s.px(theme::S8)).max(1);
        let mut y = s.px(theme::S8);
        let row = s.px(theme::ROW);

        // Headline: hero values get more room.
        let shares: Vec<f32> = self
            .last
            .as_ref()
            .map(|f| {
                f.headline
                    .iter()
                    .map(|h| if h.hero { 1.4 } else { 1.0 })
                    .collect()
            })
            .unwrap_or_else(|| vec![1.0; self.head.len()]);
        let total: f32 = shares.iter().sum::<f32>().max(1.0);
        let hero = self.fonts.get(Font::Hero);
        let sec = self.fonts.get(Font::Secondary);
        let mut x = x0 as f32;
        for (i, &(l, v)) in self.head.iter().enumerate() {
            let share = shares.get(i).copied().unwrap_or(1.0);
            let gw = cw as f32 * share / total;
            let is_hero = share > 1.0;
            let f = if is_hero { hero } else { sec };
            place(l, x as i32, y, gw as i32 - s.px(theme::S8), row);
            let vy = y + row + s.px(theme::S4) + (hero.ascent - f.ascent);
            place(v, x as i32, vy, gw as i32 - s.px(theme::S8), f.height);
            x += gw;
        }
        y += row + s.px(theme::S4) + hero.height + s.px(theme::S16);

        for (k, (id, height)) in [
            (ChartId::Throughput, theme::THROUGHPUT_HEIGHT),
            (ChartId::Latency, theme::LATENCY_HEIGHT),
        ]
        .into_iter()
        .enumerate()
        {
            let mut lx = x0 + s.px(crate::view::palette::AXIS_WIDTH);
            for &h in &self.legends[k] {
                let lw = text::get(h)
                    .map(|st| self.fonts.measure(Font::Small, &st.a))
                    .unwrap_or(0)
                    + s.px(theme::S12 + theme::S8);
                place(h, lx, y, lw, row);
                lx += lw + s.px(theme::S16);
            }
            y += row;
            let ch = s.px(height);
            self.place_chart(id, x0, y, cw, ch);
            y += ch + s.px(theme::S8);
        }
        let oh = s.px(theme::OVERVIEW_HEIGHT);
        self.place_chart(ChartId::Overview, x0, y, cw, oh);
        y += oh + s.px(theme::S24);

        // Breakdowns in four columns.
        let gap = s.px(theme::S24);
        let n = self.columns.len().max(1) as i32;
        let colw = ((cw - (n - 1) * gap) / n).max(1);
        let mut bottom = y;
        let mut boxes = Vec::new();
        for (i, g) in self.columns.iter().enumerate() {
            let cx = x0 + i as i32 * (colw + gap);
            let mut cy = y;
            place(g.title, cx, cy, colw, s.px(theme::TITLE_ROW));
            cy += s.px(theme::TITLE_ROW + theme::S4);
            for (shape, it) in g.shape.iter().zip(&g.items) {
                match (shape, it) {
                    (Shape::Boxes(id, rows), ItemCtl::Chart(_)) => {
                        let bh = s.px(boxplot::height(*rows));
                        boxes.push((*id, cx, cy, colw, bh));
                        cy += bh + s.px(theme::S8);
                    }
                    (_, ItemCtl::Static(h)) => {
                        cy += self.place_kv(*h, cx, cy, colw);
                    }
                    _ => {}
                }
            }
            bottom = bottom.max(cy);
        }
        for (id, bx, by, bw, bh) in boxes {
            self.place_chart(ChartId::Box(id), bx, by, bw, bh);
        }
        y = bottom + gap;
        self.log.place(s, x0, y, cw);
        y += s.px(theme::TAB_HEIGHT + theme::S4 + theme::LOG_HEIGHT + theme::S16);
        y
    }

    /// Places a label or key/value static; returns its height.
    fn place_kv(&self, h: HWND, x: i32, y: i32, w: i32) -> i32 {
        let s = self.scale;
        let row = s.px(theme::ROW);
        let Some(st) = text::get(h) else {
            return 0;
        };
        let ht = match st.look {
            Look::Kv { .. } => {
                let two = text::kv_two_lines(&self.fonts, s, &st.a, &st.b, w);
                text::update(h, |x| x.look = Look::Kv { two_lines: two });
                if two { 2 * row } else { row }
            }
            _ => row,
        };
        place(h, x, y, w, ht);
        ht
    }

    fn layout_side(&mut self, pw: i32) -> i32 {
        let s = self.scale;
        let x0 = s.px(theme::S8);
        let cw = (pw - x0 - s.px(theme::S16)).max(1);
        let mut y = s.px(theme::S8);
        let rssi_after = self
            .last
            .as_ref()
            .and_then(|f| f.info.iter().position(|sec| sec.title == "Wi-Fi"))
            .unwrap_or(self.info.len().saturating_sub(1));
        let mut rssi_at = None;
        for (i, g) in self.info.iter().enumerate() {
            place(g.title, x0, y, cw, s.px(theme::TITLE_ROW));
            y += s.px(theme::TITLE_ROW + theme::S4);
            for it in &g.items {
                if let ItemCtl::Static(h) = it {
                    y += self.place_kv(*h, x0, y, cw);
                }
            }
            if i == rssi_after {
                y += s.px(theme::S4);
                rssi_at = Some(y);
                if self.charts.iter().any(|c| c.id == ChartId::Rssi) {
                    y += s.px(rssi::HEIGHT) + s.px(theme::S8);
                }
            }
            y += s.px(theme::S16);
        }
        if let Some(ry) = rssi_at {
            self.place_chart(ChartId::Rssi, x0, ry, cw, s.px(rssi::HEIGHT));
        }
        y
    }

    /// Sizes a page to its content and updates its scroll bar.
    fn set_page(&mut self, which: usize, width: i32, content: i32) {
        let sc = if which == 0 {
            &mut self.main
        } else {
            &mut self.side
        };
        let mut r = RECT::default();
        // SAFETY: our own window.
        let _ = unsafe { GetClientRect(sc.hwnd, &raw mut r) };
        sc.view = r.bottom;
        sc.content = content;
        let max_pos = (content - sc.view).max(0);
        sc.pos = sc.pos.clamp(0, max_pos);
        place(sc.page, 0, -sc.pos, width, content.max(sc.view));
        let si = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: content.max(1) - 1,
            nPage: sc.view.max(0) as u32,
            nPos: sc.pos,
            nTrackPos: 0,
        };
        // SAFETY: our own scroll bar.
        unsafe { SetScrollInfo(sc.hwnd, SB_VERT, &raw const si, true) };
    }

    fn scroll_to(&mut self, which: usize, to: i32) {
        let sc = if which == 0 {
            &mut self.main
        } else {
            &mut self.side
        };
        let to = to.clamp(0, (sc.content - sc.view).max(0));
        if to == sc.pos {
            return;
        }
        sc.pos = to;
        let mut r = RECT::default();
        // SAFETY: our own windows.
        unsafe {
            let _ = GetClientRect(sc.page, &raw mut r);
            let _ = SetWindowPos(
                sc.page,
                None,
                0,
                -to,
                r.right,
                r.bottom,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let si = SCROLLINFO {
                cbSize: size_of::<SCROLLINFO>() as u32,
                fMask: SIF_POS,
                nPos: to,
                ..Default::default()
            };
            SetScrollInfo(sc.hwnd, SB_VERT, &raw const si, true);
        }
        self.popup.hide();
        self.update_charts();
    }

    /// Whether a chart on a page is inside its scroller's view.
    pub fn chart_visible(&self, c: &Chart) -> bool {
        // SAFETY: queries on our own windows.
        let shown =
            unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() };
        if !shown {
            return false;
        }
        let sc = if c.page == self.main.page {
            &self.main
        } else {
            &self.side
        };
        let (top, bottom) = (c.rect.1, c.rect.1 + c.rect.3);
        bottom > sc.pos && top < sc.pos + sc.view
    }
}

fn client_width(h: HWND) -> i32 {
    let mut r = RECT::default();
    // SAFETY: our own window.
    let _ = unsafe { GetClientRect(h, &raw mut r) };
    r.right
}

/// The notification-area tooltip: the latency value, or the app's name
/// before there is one.
fn tray_tip(title: &str) -> String {
    if title.is_empty() {
        APP_NAME.to_owned()
    } else {
        title.to_owned()
    }
}

// ---- Export ----

fn export() {
    let Some((owner, name)) =
        with_app(|a| a.last.as_ref().map(|f| (a.hwnd, f.export_name.clone()))).flatten()
    else {
        return;
    };
    match save_dialog(owner, &name) {
        Ok(Some(path)) => {
            with_app(|a| {
                a.core.export(path);
                a.refresh();
            });
        }
        Ok(None) => {}
        Err(e) => {
            with_app(|a| {
                text::update(a.status, |s| {
                    s.a = format!("Export failed: {}", e.message());
                    if let Look::Label { color, .. } = &mut s.look {
                        *color = Color::Error;
                    }
                });
            });
        }
    }
}

/// Asks where to save, through the system save dialog. The dialog keeps
/// its last folder and size in the user's registry on its own; it is told
/// not to add the file to Recent items.
fn save_dialog(owner: HWND, name: &str) -> windows::core::Result<Option<PathBuf>> {
    // SAFETY: COM calls on the UI thread, which initialised COM.
    unsafe {
        let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        let opts = d.GetOptions()?;
        d.SetOptions(opts | FOS_DONTADDTORECENT | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)?;
        let spec = [COMDLG_FILTERSPEC {
            pszName: w!("JSON"),
            pszSpec: w!("*.json"),
        }];
        d.SetFileTypes(&spec)?;
        d.SetDefaultExtension(w!("json"))?;
        let name = wide(name);
        d.SetFileName(PCWSTR(name.as_ptr()))?;
        if let Err(e) = d.Show(Some(owner)) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(e);
        }
        let item = d.GetResult()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(Some(PathBuf::from(
            s.map_err(|_| windows::core::Error::empty())?,
        )))
    }
}
