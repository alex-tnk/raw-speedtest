//! The log: a tab strip over a virtual list view of requests, connections
//! and errors, newest first. Rows are fetched from the core a page at a
//! time as the list asks for them.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DrawFocusRect, DrawTextW,
    EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NEWFONT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, LIST_VIEW_ITEM_FLAGS, LVCF_FMT, LVCF_SUBITEM,
    LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT, LVCFMT_RIGHT, LVCOLUMNW, LVIF_TEXT, LVIR_BOUNDS,
    LVM_DELETECOLUMN, LVM_ENSUREVISIBLE, LVM_GETHEADER, LVM_GETITEMRECT, LVM_GETTOPINDEX,
    LVM_INSERTCOLUMNW, LVM_SCROLL, LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMCOUNT,
    LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVN_GETDISPINFOW, LVN_GETINFOTIPW, LVN_ODCACHEHINT,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_OWNERDATA, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSICF_NOSCROLL, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR,
    NMLVCACHEHINT, NMLVCUSTOMDRAW, NMLVDISPINFOW, NMLVGETINFOTIPW, TCIF_TEXT, TCITEMW,
    TCM_GETCURSEL, TCM_GETITEMCOUNT, TCM_GETITEMRECT, TCM_GETITEMW, TCM_INSERTITEMW, TCM_SETITEMW,
    TCN_SELCHANGE, WC_LISTVIEWW, WC_TABCONTROLW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_NCDESTROY, WM_NOTIFY, WM_PAINT, WS_CHILD,
    WS_CLIPSIBLINGS, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::PWSTR;

use super::app::tip_text;
use super::text::{self, Font};
use super::theme::{self, Theme};
use super::win::{self, Scale, copy_into, place, send, wide};
use crate::view::core::Core;
use crate::view::frame::LogCounts;
use crate::view::log::{self, Row, Tab};
use crate::view::palette::Color;

const ID_TABS: usize = 300;
const ID_LIST: usize = 301;
/// The list shows at most this many of the newest rows.
const MAX_ROWS: usize = 20_000;
/// Rows fetched at once.
const PAGE: usize = 64;
const TABS: [Tab; 3] = [Tab::Requests, Tab::Connections, Tab::Errors];

struct Cache {
    tab: Tab,
    base: usize,
    first: usize,
    rows: Vec<Row>,
}

pub struct LogView {
    pub tabs: HWND,
    pub list: HWND,
    tab: Tab,
    counts: [usize; 3],
    /// The session length the list's rows are counted from; rows are
    /// newest first, so a longer session shifts every index.
    base: usize,
    cache: Option<Cache>,
    scale: Scale,
}

impl LogView {
    pub fn new(page: HWND, scale: Scale) -> LogView {
        let tabs = win::create(
            WINDOW_EX_STYLE::default(),
            WC_TABCONTROLW,
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS,
            Some(page),
            ID_TABS,
            (0, 0, 0, 0),
        );
        for (i, t) in TABS.iter().enumerate() {
            let mut s = wide(&t.label(0));
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(s.as_mut_ptr()),
                ..Default::default()
            };
            send(tabs, TCM_INSERTITEMW, i, (&raw const item) as isize);
        }
        // SAFETY: subclassing our own control; removed at WM_NCDESTROY.
        let _ = unsafe { SetWindowSubclass(tabs, Some(tab_proc), 1, 0) };
        let list = win::create(
            WINDOW_EX_STYLE::default(),
            WC_LISTVIEWW,
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_SINGLESEL),
            Some(page),
            ID_LIST,
            (0, 0, 0, 0),
        );
        let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_INFOTIP;
        send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex as isize);
        // SAFETY: as above.
        let _ = unsafe { SetWindowSubclass(list, Some(list_proc), 1, 0) };
        let lv = LogView {
            tabs,
            list,
            tab: Tab::Requests,
            counts: [0; 3],
            base: 0,
            cache: None,
            scale,
        };
        lv.columns();
        lv
    }

    pub fn header(&self) -> HWND {
        HWND(send(self.list, LVM_GETHEADER, 0, 0).0 as *mut _)
    }

    fn columns(&self) {
        while send(self.list, LVM_DELETECOLUMN, 0, 0).0 != 0 {}
        for (i, c) in log::columns(self.tab).iter().enumerate() {
            let mut title = wide(c.title);
            let col = LVCOLUMNW {
                mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT | LVCF_SUBITEM,
                // The first column can't be right-aligned; it is the time.
                fmt: if c.numeric && i > 0 {
                    LVCFMT_RIGHT
                } else {
                    LVCFMT_LEFT
                },
                cx: self.scale.px(c.width + theme::S16),
                pszText: PWSTR(title.as_mut_ptr()),
                iSubItem: i as i32,
                ..Default::default()
            };
            send(self.list, LVM_INSERTCOLUMNW, i, (&raw const col) as isize);
        }
    }

    pub fn place(&self, s: Scale, x: i32, y: i32, w: i32) {
        let th = s.px(theme::TAB_HEIGHT);
        place(self.tabs, x, y, w, th);
        place(
            self.list,
            x,
            y + th + s.px(theme::S4),
            w,
            s.px(theme::LOG_HEIGHT),
        );
    }

    pub fn set_scale(&mut self, s: Scale) {
        self.scale = s;
        self.columns();
    }

    pub fn apply_theme(&self, t: &Theme) {
        let bg = t.colorref(Color::Background).0 as isize;
        send(self.list, LVM_SETBKCOLOR, 0, bg);
        send(self.list, LVM_SETTEXTBKCOLOR, 0, bg);
        send(
            self.list,
            LVM_SETTEXTCOLOR,
            0,
            t.colorref(Color::Text).0 as isize,
        );
        for h in [self.tabs, self.list] {
            // SAFETY: our own controls.
            unsafe {
                let _ = InvalidateRect(Some(h), None, true);
            }
        }
    }

    /// New row counts from a frame.
    pub fn update(&mut self, counts: LogCounts) {
        let new = [counts.requests, counts.connections, counts.errors];
        for (i, t) in TABS.iter().enumerate() {
            if new[i] != self.counts[i] {
                let mut s = wide(&t.label(new[i]));
                let item = TCITEMW {
                    mask: TCIF_TEXT,
                    pszText: PWSTR(s.as_mut_ptr()),
                    ..Default::default()
                };
                send(self.tabs, TCM_SETITEMW, i, (&raw const item) as isize);
            }
        }
        self.counts = new;
        self.sync();
    }

    /// Sets the list's row count; rows already scrolled to stay in view
    /// as newer ones arrive above them.
    fn sync(&mut self) {
        let len = self.counts[self.tab as usize];
        if len == self.base {
            return;
        }
        let added = len.saturating_sub(self.base);
        let top = send(self.list, LVM_GETTOPINDEX, 0, 0).0;
        self.base = len;
        self.cache = None;
        send(
            self.list,
            LVM_SETITEMCOUNT,
            len.min(MAX_ROWS),
            LVSICF_NOSCROLL as isize,
        );
        if top > 0 && added > 0 {
            let mut r = RECT {
                left: LVIR_BOUNDS as i32,
                ..Default::default()
            };
            send(self.list, LVM_GETITEMRECT, 0, (&raw mut r) as isize);
            let dy = win::height(&r) * added.min(MAX_ROWS) as i32;
            send(self.list, LVM_SCROLL, 0, dy as isize);
        }
        // SAFETY: our own control.
        unsafe {
            let _ = InvalidateRect(Some(self.list), None, false);
        }
    }

    fn select(&mut self) {
        let i = send(self.tabs, TCM_GETCURSEL, 0, 0).0;
        let Some(&tab) = usize::try_from(i).ok().and_then(|i| TABS.get(i)) else {
            return;
        };
        self.tab = tab;
        self.base = usize::MAX;
        self.cache = None;
        self.columns();
        self.sync();
        send(self.list, LVM_ENSUREVISIBLE, 0, 0);
        // SAFETY: our own controls.
        unsafe {
            let _ = InvalidateRect(Some(self.tabs), None, false);
        }
    }

    fn fetch(&mut self, core: &Core, first: usize, count: usize) {
        // Rows that arrived since the count was set sit above row 0.
        let delta = core.log_len(self.tab).saturating_sub(self.base);
        let rows = core.log_rows(self.tab, first + delta, count);
        self.cache = Some(Cache {
            tab: self.tab,
            base: self.base,
            first,
            rows,
        });
    }

    fn row(&mut self, core: &Core, i: usize) -> Option<&Row> {
        let hit = self.cache.as_ref().is_some_and(|c| {
            c.tab == self.tab && c.base == self.base && i >= c.first && i < c.first + c.rows.len()
        });
        if !hit {
            self.fetch(core, i.saturating_sub(PAGE / 4), PAGE);
        }
        let c = self.cache.as_ref()?;
        c.rows.get(i.checked_sub(c.first)?)
    }

    pub fn notify(&mut self, core: &Core, theme: &Theme, l: LPARAM) -> Option<LRESULT> {
        // SAFETY: WM_NOTIFY's LPARAM is an NMHDR, extended per code below.
        let hdr = unsafe { &*(l.0 as *const NMHDR) };
        match hdr.code {
            TCN_SELCHANGE if hdr.hwndFrom == self.tabs => {
                self.select();
                Some(LRESULT(0))
            }
            LVN_GETDISPINFOW => {
                // SAFETY: the list view's display-info request.
                let di = unsafe { &mut *(l.0 as *mut NMLVDISPINFOW) };
                if di.item.mask & LVIF_TEXT != LIST_VIEW_ITEM_FLAGS(0)
                    && !di.item.pszText.is_null()
                    && di.item.cchTextMax > 0
                {
                    let (i, sub) = (di.item.iItem as usize, di.item.iSubItem as usize);
                    let text = self
                        .row(core, i)
                        .and_then(|r| r.cells.get(sub))
                        .map(|c| c.text.clone())
                        .unwrap_or_default();
                    // SAFETY: the buffer is cchTextMax characters long.
                    let out = unsafe {
                        std::slice::from_raw_parts_mut(
                            di.item.pszText.0,
                            di.item.cchTextMax as usize,
                        )
                    };
                    copy_into(&text, out);
                }
                Some(LRESULT(0))
            }
            LVN_ODCACHEHINT => {
                // SAFETY: the list view's cache hint.
                let ch = unsafe { &*(l.0 as *const NMLVCACHEHINT) };
                let (from, to) = (ch.iFrom.max(0) as usize, ch.iTo.max(0) as usize);
                let covered = self.cache.as_ref().is_some_and(|c| {
                    c.tab == self.tab
                        && c.base == self.base
                        && from >= c.first
                        && to < c.first + c.rows.len()
                });
                if !covered {
                    self.fetch(core, from, (to + 1 - from.min(to + 1)).max(PAGE));
                }
                Some(LRESULT(0))
            }
            LVN_GETINFOTIPW => {
                // SAFETY: the list view's info-tip request.
                let it = unsafe { &mut *(l.0 as *mut NMLVGETINFOTIPW) };
                if !it.pszText.is_null() && it.cchTextMax > 0 {
                    let tip = self
                        .row(core, it.iItem.max(0) as usize)
                        .and_then(|r| r.tip.as_ref())
                        .map(tip_text);
                    if let Some(t) = tip {
                        // SAFETY: the buffer is cchTextMax characters long.
                        let out = unsafe {
                            std::slice::from_raw_parts_mut(it.pszText.0, it.cchTextMax as usize)
                        };
                        copy_into(&t, out);
                    }
                }
                Some(LRESULT(0))
            }
            NM_CUSTOMDRAW if hdr.hwndFrom == self.list => {
                // SAFETY: the list view's custom-draw notification.
                let cd = unsafe { &mut *(l.0 as *mut NMLVCUSTOMDRAW) };
                let stage = cd.nmcd.dwDrawStage;
                let r = if stage == CDDS_PREPAINT {
                    CDRF_NOTIFYITEMDRAW
                } else if stage == CDDS_ITEMPREPAINT {
                    CDRF_NOTIFYSUBITEMDRAW
                } else if stage.0 == CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0 {
                    let color = self
                        .row(core, cd.nmcd.dwItemSpec)
                        .and_then(|r| r.cells.get(cd.iSubItem.max(0) as usize))
                        .map_or(Color::Text, |c| c.color);
                    cd.clrText = theme.colorref(color);
                    CDRF_NEWFONT
                } else {
                    CDRF_DODEFAULT
                };
                Some(LRESULT(r as isize))
            }
            _ => None,
        }
    }
}

/// Draws the tab strip flat in the window's colours: the selected tab in
/// primary text with an accent underline, the others in secondary text.
unsafe extern "system" fn tab_proc(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_tabs(h);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: removing our own subclass.
            unsafe {
                let _ = RemoveWindowSubclass(h, Some(tab_proc), 1);
                DefSubclassProc(h, m, w, l)
            }
        }
        // SAFETY: default handling.
        _ => unsafe { DefSubclassProc(h, m, w, l) },
    }
}

fn paint_tabs(h: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: painting our own control.
    let dc = unsafe { BeginPaint(h, &raw mut ps) };
    if let Some(p) = text::paint() {
        let t = &p.theme;
        let mut rc = RECT::default();
        // SAFETY: drawing into the paint DC.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(h, &raw mut rc);
            FillRect(dc, &raw const rc, p.bg);
            SelectObject(dc, p.font(Font::Ui).font.into());
            SetBkMode(dc, TRANSPARENT);
        }
        let n = send(h, TCM_GETITEMCOUNT, 0, 0).0.max(0) as usize;
        let sel = send(h, TCM_GETCURSEL, 0, 0).0;
        let mark = p.scale.px(theme::TAB_MARK).max(1);
        let pad = p.scale.px(theme::S4);
        for i in 0..n {
            let mut r = RECT::default();
            send(h, TCM_GETITEMRECT, i, (&raw mut r) as isize);
            let mut buf = [0u16; 64];
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(buf.as_mut_ptr()),
                cchTextMax: buf.len() as i32,
                ..Default::default()
            };
            send(h, TCM_GETITEMW, i, (&raw mut item) as isize);
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let selected = i as isize == sel;
            // SAFETY: drawing into the paint DC.
            unsafe {
                SetTextColor(
                    dc,
                    t.colorref(if selected { Color::Text } else { Color::Text2 }),
                );
                let mut tr = r;
                DrawTextW(
                    dc,
                    &mut buf[..len],
                    &raw mut tr,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                if selected {
                    let u = RECT {
                        left: r.left + pad,
                        top: r.bottom - mark,
                        right: r.right - pad,
                        bottom: r.bottom,
                    };
                    let b =
                        windows::Win32::Graphics::Gdi::CreateSolidBrush(t.colorref(Color::Accent));
                    FillRect(dc, &raw const u, b);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(b.into());
                    if GetFocus() == h {
                        let f = RECT {
                            bottom: r.bottom - mark - 1,
                            ..r
                        };
                        let _ = DrawFocusRect(dc, &raw const f);
                    }
                }
            }
        }
    }
    // SAFETY: matches BeginPaint.
    unsafe {
        let _ = EndPaint(h, &raw const ps);
    }
}

/// Colours the list header's titles to match the theme.
unsafe extern "system" fn list_proc(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if m == WM_NOTIFY {
        // SAFETY: WM_NOTIFY's LPARAM is an NMHDR.
        let hdr = unsafe { &*(l.0 as *const NMHDR) };
        let header = HWND(send(h, LVM_GETHEADER, 0, 0).0 as *mut _);
        if hdr.code == NM_CUSTOMDRAW && hdr.hwndFrom == header {
            // SAFETY: the header's custom-draw notification.
            let cd = unsafe { &*(l.0 as *const NMCUSTOMDRAW) };
            if cd.dwDrawStage == CDDS_PREPAINT {
                return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
            }
            if cd.dwDrawStage == CDDS_ITEMPREPAINT {
                if let Some(p) = text::paint() {
                    // SAFETY: the header's paint DC.
                    unsafe { SetTextColor(cd.hdc, p.theme.colorref(Color::Text2)) };
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
        }
    }
    if m == WM_NCDESTROY {
        // SAFETY: removing our own subclass.
        unsafe {
            let _ = RemoveWindowSubclass(h, Some(list_proc), 1);
        }
    }
    // SAFETY: default handling.
    unsafe { DefSubclassProc(h, m, w, l) }
}
