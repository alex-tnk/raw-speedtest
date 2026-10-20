//! Chart child windows drawn with Direct2D, their hover, zoom and pan, and
//! the hover tooltip popup.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, GetMonitorInfoW, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HTTRANSPARENT, HWND_TOPMOST, KillTimer, MA_NOACTIVATE, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetTimer, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_ERASEBKGND,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCHITTEST, WM_PAINT, WM_TIMER, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use super::app::{App, dispatch, with_app};
use super::gfx::{Target, TipLayout};
use super::theme;
use super::win::{self, Scale, mouse_xy};
use crate::view::chart::Hover;
use crate::view::core::ChartId;
use crate::view::palette::Color;
use crate::view::scene::{Point, Prim, Scene};
use crate::view::tip::Tip;

pub const CLASS: PCWSTR = w!("RstChart");
pub const TIP_CLASS: PCWSTR = w!("RstTip");

// Wheel key state flags (winuser.h).
const MK_SHIFT: u32 = 0x0004;
const MK_CONTROL: u32 = 0x0008;

/// Repaint later: a paint arrived while the app was busy.
const TIMER_REPAINT: usize = 1;

pub struct Chart {
    pub hwnd: HWND,
    pub id: ChartId,
    /// The page the chart sits on.
    pub page: HWND,
    /// Position and size on the page, in pixels.
    pub rect: (i32, i32, i32, i32),
    /// Size in DIPs, as handed to the core.
    pub w: f32,
    pub h: f32,
    /// Fingerprint of the scene last seen; 0 is unknown.
    pub fp: u64,
    target: Option<Target>,
    hover: Option<Hover>,
    mouse: Option<(f32, f32)>,
    drag: Option<f32>,
    tracking: bool,
}

impl Chart {
    pub fn new(page: HWND, id: ChartId) -> Chart {
        let hwnd = win::create(
            WINDOW_EX_STYLE::default(),
            CLASS,
            "",
            WS_CHILD | WS_VISIBLE,
            Some(page),
            0,
            (0, 0, 0, 0),
        );
        Chart {
            hwnd,
            id,
            page,
            rect: (0, 0, 0, 0),
            w: 0.0,
            h: 0.0,
            fp: 0,
            target: None,
            hover: None,
            mouse: None,
            drag: None,
            tracking: false,
        }
    }

    pub fn set_rect(&mut self, s: Scale, x: i32, y: i32, w: i32, h: i32) {
        let r = (x, y, w.max(1), h.max(1));
        if r != self.rect {
            win::place(self.hwnd, r.0, r.1, r.2, r.3);
            self.rect = r;
        }
        self.w = s.dip(r.2);
        self.h = s.dip(r.3);
    }

    /// Throughput and latency share a zoomable, pannable time window.
    fn zoomable(&self) -> bool {
        matches!(self.id, ChartId::Throughput | ChartId::Latency)
    }

    fn timeline(&self) -> bool {
        self.zoomable() || self.id == ChartId::Overview
    }
}

/// A cheap fingerprint of a scene, to repaint only when it changed.
fn fingerprint(sc: &Scene) -> u64 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut mix = |v: u64| h = (h.rotate_left(5) ^ v).wrapping_mul(0x517C_C1B7_2722_0A95);
    let f = |x: f32| u64::from(x.to_bits());
    let pt = |p: Point| (f(p.x) << 32) | f(p.y);
    mix(sc.prims.len() as u64);
    mix(pt(Point {
        x: sc.plot.x0,
        y: sc.plot.y0,
    }));
    mix(pt(Point {
        x: sc.plot.x1,
        y: sc.plot.y1,
    }));
    for p in &sc.pts {
        mix(pt(*p));
    }
    for c in sc.text.as_bytes().chunks(8) {
        let mut b = [0u8; 8];
        b[..c.len()].copy_from_slice(c);
        mix(u64::from_le_bytes(b));
    }
    for p in &sc.prims {
        let (kind, range, color, a, b) = match p {
            Prim::Line {
                range,
                color,
                alpha,
                width,
                dashed,
            } => (
                u64::from(*dashed),
                range.clone(),
                *color,
                f(*alpha),
                f(*width),
            ),
            Prim::Area {
                range,
                base,
                color,
                alpha,
            } => (2, range.clone(), *color, f(*alpha), f(*base)),
            Prim::Rect {
                rect,
                color,
                alpha,
                filled,
            } => {
                mix(pt(Point {
                    x: rect.x0,
                    y: rect.y0,
                }));
                mix(pt(Point {
                    x: rect.x1,
                    y: rect.y1,
                }));
                (3 + u64::from(*filled), 0..0, *color, f(*alpha), 0)
            }
            Prim::Dots {
                range,
                radius,
                color,
                alpha,
            } => (5, range.clone(), *color, f(*alpha), f(*radius)),
            Prim::Text {
                at,
                range,
                size,
                color,
                h: ha,
                v,
            } => {
                mix(pt(*at));
                (
                    6 | (*size as u64) << 8 | (*ha as u64) << 16 | (*v as u64) << 24,
                    range.clone(),
                    *color,
                    0,
                    0,
                )
            }
        };
        mix(kind | (color as u64) << 32);
        mix((u64::from(range.start) << 32) | u64::from(range.end));
        mix((a << 32) | b);
    }
    h.max(1)
}

pub unsafe extern "system" fn chart_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            if with_app(|a| a.paint_chart(h)).is_none() {
                // The app is busy further up the stack: validate now and
                // paint again shortly.
                let mut ps = PAINTSTRUCT::default();
                // SAFETY: painting our own window.
                unsafe {
                    BeginPaint(h, &raw mut ps);
                    let _ = EndPaint(h, &raw const ps);
                    SetTimer(Some(h), TIMER_REPAINT, 10, None);
                }
            }
            return LRESULT(0);
        }
        WM_TIMER if w.0 == TIMER_REPAINT => {
            // SAFETY: our own timer and window.
            unsafe {
                let _ = KillTimer(Some(h), TIMER_REPAINT);
                let _ = InvalidateRect(Some(h), None, false);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    dispatch(h, m, w, l, App::chart_msg)
}

pub unsafe extern "system" fn tip_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            if with_app(|a| a.paint_tip()).is_none() {
                let mut ps = PAINTSTRUCT::default();
                // SAFETY: painting our own window.
                unsafe {
                    BeginPaint(h, &raw mut ps);
                    let _ = EndPaint(h, &raw const ps);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    dispatch(h, m, w, l, |_, _, _, _, _| None)
}

fn invalidate(h: HWND) {
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h), None, false);
    }
}

/// The hover tooltip: a borderless popup that never takes focus or the
/// mouse, shown at once next to the hovered point.
pub struct Popup {
    pub hwnd: HWND,
    /// The chart whose hover it shows.
    pub owner: Option<HWND>,
    tip: Option<Tip>,
    layout: Option<TipLayout>,
    target: Option<Target>,
    px: (u32, u32),
}

impl Popup {
    pub fn new(main: HWND) -> Popup {
        let hwnd = win::create(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            TIP_CLASS,
            "",
            WS_POPUP,
            Some(main),
            0,
            (0, 0, 0, 0),
        );
        Popup {
            hwnd,
            owner: None,
            tip: None,
            layout: None,
            target: None,
            px: (0, 0),
        }
    }

    pub fn hide(&mut self) {
        if self.owner.take().is_some() {
            // SAFETY: hiding our own window.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

impl App {
    pub fn chart_msg(&mut self, h: HWND, m: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        let i = self.charts.iter().position(|c| c.hwnd == h)?;
        let s = self.scale;
        let (px, py) = mouse_xy(l);
        let (x, y) = (s.dip(px), s.dip(py));
        match m {
            WM_MOUSEMOVE => {
                let c = &mut self.charts[i];
                if !c.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    // SAFETY: tracking our own window.
                    let _ = unsafe { TrackMouseEvent(&raw mut tme) };
                    c.tracking = true;
                }
                if let Some(last) = c.drag {
                    let (id, cw) = (c.id, c.w);
                    c.drag = Some(x);
                    if c.zoomable() {
                        self.core.pan(x - last, cw);
                    } else if id == ChartId::Overview {
                        self.core.center(x, cw);
                    }
                }
                self.charts[i].mouse = Some((x, y));
                self.update_charts();
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                let c = &mut self.charts[i];
                c.tracking = false;
                c.mouse = None;
                if c.hover.take().is_some() {
                    invalidate(h);
                }
                if self.popup.owner == Some(h) {
                    self.popup.hide();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let c = &mut self.charts[i];
                if c.timeline() {
                    c.drag = Some(x);
                    // SAFETY: capturing to our own window.
                    unsafe { SetCapture(h) };
                    if c.id == ChartId::Overview {
                        let cw = c.w;
                        self.core.center(x, cw);
                        self.update_charts();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.charts[i].drag.take().is_some() {
                    // SAFETY: releasing our own capture.
                    let _ = unsafe { ReleaseCapture() };
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.charts[i].drag = None;
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                if self.charts[i].timeline() {
                    self.core.reset_view();
                    self.update_charts();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let c = &self.charts[i];
                if !c.zoomable() {
                    // Plain scrolling goes to the page.
                    return None;
                }
                let keys = (w.0 & 0xFFFF) as u32;
                let notches = f64::from((w.0 >> 16) as u16 as i16) / 120.0;
                let cw = c.w;
                let step = notches as f32 * theme::WHEEL_LINE;
                if m == WM_MOUSEHWHEEL {
                    self.core.pan(-step, cw);
                } else if keys & MK_CONTROL != 0 {
                    // Wheel coordinates are on the screen.
                    let mut p = POINT { x: px, y: py };
                    // SAFETY: converting to our own window's coordinates.
                    let _ = unsafe { ScreenToClient(h, &raw mut p) };
                    self.core
                        .zoom(theme::ZOOM_STEP.powf(notches), s.dip(p.x), cw);
                } else if keys & MK_SHIFT != 0 {
                    self.core.pan(step, cw);
                } else {
                    return None;
                }
                self.update_charts();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// Repaints charts whose scene changed and refreshes hovers, so a still
    /// pointer follows live data.
    pub fn update_charts(&mut self) {
        let label_h = self.gfx.label_h;
        for i in 0..self.charts.len() {
            if !self.chart_visible(&self.charts[i]) {
                self.charts[i].fp = 0;
                continue;
            }
            let (id, w, h, mouse) = {
                let c = &self.charts[i];
                (c.id, c.w, c.h, c.mouse)
            };
            if w < 1.0 || h < 1.0 {
                continue;
            }
            let fp = fingerprint(self.core.scene(id, w, h, label_h));
            let hover = mouse.map(|(x, y)| self.core.hover(id, w, h, x, y));
            let c = &mut self.charts[i];
            let mut dirty = fp != c.fp;
            c.fp = fp;
            let hwnd = c.hwnd;
            if mouse.is_some() && c.hover != hover {
                dirty = true;
                c.hover = hover.clone();
                match hover.and_then(|hv| hv.tip.map(|t| (t, hv.anchor))) {
                    Some((tip, anchor)) => self.show_tip(hwnd, tip, anchor),
                    None => {
                        if self.popup.owner == Some(hwnd) {
                            self.popup.hide();
                        }
                    }
                }
            }
            if dirty {
                invalidate(hwnd);
            }
        }
    }

    pub fn paint_chart(&mut self, h: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: painting our own window.
        unsafe { BeginPaint(h, &raw mut ps) };
        let App {
            charts,
            core,
            gfx,
            theme,
            scale,
            ..
        } = self;
        if let Some(c) = charts.iter_mut().find(|c| c.hwnd == h)
            && c.rect.2 > 0
            && c.rect.3 > 0
        {
            let px = (c.rect.2 as u32, c.rect.3 as u32);
            let dpi = scale.dpi();
            if c.target.is_none() {
                c.target = gfx.target(h, dpi, px).ok();
            }
            let label_h = gfx.label_h;
            if let Some(t) = &mut c.target {
                t.begin(px, dpi, theme.d2d(Color::Background, 1.0));
                let sc = core.scene(c.id, c.w, c.h, label_h);
                c.fp = fingerprint(sc);
                gfx.draw_scene(t, theme, sc, c.hover.as_ref());
                if !t.end() {
                    c.target = None;
                    invalidate(h);
                }
            }
        }
        // SAFETY: matches BeginPaint.
        unsafe {
            let _ = EndPaint(h, &raw const ps);
        }
    }

    fn show_tip(&mut self, chart: HWND, tip: Tip, anchor: Point) {
        let s = self.scale;
        if self.popup.tip.as_ref() != Some(&tip) {
            self.popup.layout = Some(self.gfx.tip_layout(&tip));
            self.popup.tip = Some(tip);
        }
        let Some(lay) = &self.popup.layout else {
            return;
        };
        let (w, h) = (s.px(lay.w), s.px(lay.h));
        let mut a = POINT {
            x: s.px(anchor.x),
            y: s.px(anchor.y),
        };
        // SAFETY: coordinate conversion and monitor lookup.
        let work = unsafe {
            let _ = ClientToScreen(chart, &raw mut a);
            let mut mi = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(MonitorFromPoint(a, MONITOR_DEFAULTTONEAREST), &raw mut mi);
            mi.rcWork
        };
        // Beside the point, flipped to the other side at the screen edge:
        // never on top of it.
        let off = s.px(theme::TIP_OFFSET);
        let mut x = a.x + off;
        if x + w > work.right {
            x = a.x - off - w;
        }
        let mut y = a.y + off;
        if y + h > work.bottom {
            y = a.y - off - h;
        }
        let x = x.max(work.left);
        let y = y.max(work.top);
        self.popup.owner = Some(chart);
        // SAFETY: positioning our own popup.
        unsafe {
            let _ = SetWindowPos(
                self.popup.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        invalidate(self.popup.hwnd);
    }

    pub fn paint_tip(&mut self) {
        let h = self.popup.hwnd;
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: painting our own window.
        unsafe { BeginPaint(h, &raw mut ps) };
        let mut r = RECT::default();
        // SAFETY: our own window.
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(h, &raw mut r) };
        let px = (r.right.max(1) as u32, r.bottom.max(1) as u32);
        let dpi = self.scale.dpi();
        let App {
            popup, gfx, theme, ..
        } = self;
        if popup.target.is_none() || popup.px != px {
            if popup.target.is_none() {
                popup.target = gfx.target(h, dpi, px).ok();
            }
            popup.px = px;
        }
        if let (Some(t), Some(lay)) = (&mut popup.target, &popup.layout) {
            t.begin(px, dpi, theme::d2d(theme.surface(), 1.0));
            gfx.draw_tip(t, theme, lay);
            if !t.end() {
                popup.target = None;
                invalidate(h);
            }
        }
        // SAFETY: matches BeginPaint.
        unsafe {
            let _ = EndPaint(h, &raw const ps);
        }
    }
}
