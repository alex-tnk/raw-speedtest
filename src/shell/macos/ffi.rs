//! The C ABI the SwiftUI shell calls, declared in `include/rst.h`. Every
//! function runs on the main thread; only the wake and quit callbacks are
//! invoked from other threads. Returned buffers are borrowed and stay valid
//! until the next call of the same function (for scenes: of the same chart).

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::json;
use crate::model::RunMode;
use crate::view::chart::boxplot::BoxId;
use crate::view::core::{ChartId, Core};
use crate::view::frame::Setting;
use crate::view::log::Tab;
use crate::view::scene::Prim;

type WakeFn = extern "C" fn(ctx: *mut c_void, delay_ms: u32);
type QuitFn = extern "C" fn(ctx: *mut c_void);

/// A callback with its context pointer, callable from any thread.
#[derive(Clone, Copy)]
struct Callback<F> {
    f: F,
    ctx: *mut c_void,
}
// SAFETY: the Swift side documents its callbacks as thread-safe; the
// context is an opaque token Swift never dereferences off the main thread.
unsafe impl<F> Send for Callback<F> {}
// SAFETY: as above.
unsafe impl<F> Sync for Callback<F> {}

impl Callback<QuitFn> {
    // A method, so closures capture the whole (Send) callback rather than
    // its raw-pointer field.
    fn quit(self) {
        (self.f)(self.ctx)
    }
}

/// The shell's wake callback, coalesced: at most one wake is outstanding
/// until the shell pulls a frame.
#[derive(Default)]
struct Wake {
    cb: Mutex<Option<Callback<WakeFn>>>,
    pending: AtomicBool,
}

impl Wake {
    fn wake(&self, delay: Duration) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let cb = *self.cb.lock().unwrap_or_else(|e| e.into_inner());
        match cb {
            Some(cb) => (cb.f)(cb.ctx, delay.as_millis().min(u128::from(u32::MAX)) as u32),
            None => self.pending.store(false, Ordering::Release),
        }
    }
}

/// One drawing command; see `include/rst.h`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RstCmd {
    pub kind: u8,
    pub color: u8,
    pub flags: u8,
    pub pad: u8,
    pub alpha: f32,
    pub a: f32,
    pub start: u32,
    pub end: u32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[repr(C)]
pub struct RstScene {
    pub cmds: *const RstCmd,
    pub n_cmds: u32,
    /// x, y pairs.
    pub pts: *const f32,
    pub n_pts: u32,
    /// UTF-8, not NUL-terminated.
    pub text: *const u8,
    pub n_text: u32,
    pub plot_x0: f32,
    pub plot_y0: f32,
    pub plot_x1: f32,
    pub plot_y1: f32,
    /// Changes whenever the scene was rebuilt.
    pub generation: u64,
}

#[repr(C)]
pub struct RstBytes {
    pub ptr: *const u8,
    pub len: usize,
}

pub struct RstCore {
    core: Core,
    wake: Arc<Wake>,
    cmds: HashMap<ChartId, (u64, Vec<RstCmd>)>,
    frame_json: String,
    hover_json: String,
    log_json: String,
    fixed_json: String,
}

impl RstCore {
    pub fn new() -> RstCore {
        let wake = Arc::new(Wake::default());
        let w = wake.clone();
        RstCore {
            core: Core::new(Arc::new(move |d| w.wake(d))),
            wake,
            cmds: HashMap::new(),
            frame_json: String::new(),
            hover_json: String::new(),
            log_json: String::new(),
            fixed_json: String::new(),
        }
    }
}

/// # Safety
/// `ptr` must be the core passed to `rst_ui_main`; main thread only.
unsafe fn core<'a>(ptr: *mut RstCore) -> Option<&'a mut RstCore> {
    // SAFETY: per the function contract.
    unsafe { ptr.as_mut() }
}

fn bytes(s: &str, out: *mut RstBytes) {
    // SAFETY: Swift passes a pointer to a local RstBytes.
    if let Some(out) = unsafe { out.as_mut() } {
        *out = RstBytes {
            ptr: s.as_ptr(),
            len: s.len(),
        };
    }
}

/// Chart kinds and box plot indices, as `include/rst.h` numbers them.
fn chart_id(kind: u32, index: u32) -> Option<ChartId> {
    Some(match kind {
        0 => ChartId::Throughput,
        1 => ChartId::Latency,
        2 => ChartId::Overview,
        3 => ChartId::Box(match index {
            0 => BoxId::Download,
            1 => BoxId::Upload,
            2 => BoxId::Latency,
            3 => BoxId::Gateway,
            4 => BoxId::Udp,
            _ => return None,
        }),
        4 => ChartId::Rssi,
        _ => return None,
    })
}

fn log_tab(v: u32) -> Option<Tab> {
    match v {
        0 => Some(Tab::Requests),
        1 => Some(Tab::Connections),
        2 => Some(Tab::Errors),
        _ => None,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_set_wake(ptr: *mut RstCore, f: Option<WakeFn>, ctx: *mut c_void) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    *c.wake.cb.lock().unwrap_or_else(|e| e.into_inner()) = f.map(|f| Callback { f, ctx });
    c.wake.pending.store(false, Ordering::Release);
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_set_quit(_ptr: *mut RstCore, f: Option<QuitFn>, ctx: *mut c_void) {
    if let Some(f) = f {
        let cb = Callback { f, ctx };
        crate::platform::set_quit_hook(Box::new(move || cb.quit()));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_forget_preferences(_ptr: *mut RstCore) {
    crate::platform::current().forget_preferences();
}

/// Data colours, log columns and chart constants, as JSON.
#[unsafe(no_mangle)]
pub extern "C" fn rst_fixed(ptr: *mut RstCore, out: *mut RstBytes) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    json::fixed(&mut c.fixed_json);
    bytes(&c.fixed_json, out);
}

/// The frame as JSON. Clears the outstanding wake.
#[unsafe(no_mangle)]
pub extern "C" fn rst_frame(ptr: *mut RstCore, out: *mut RstBytes) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    c.wake.pending.store(false, Ordering::Release);
    let f = c.core.frame();
    let tick = c
        .core
        .next_tick()
        .map(|d| d.as_millis().min(u128::from(u32::MAX)) as u32);
    json::frame(&f, tick, &mut c.frame_json);
    bytes(&c.frame_json, out);
}

/// The scene of a chart at `w`×`h` points. Kinds: 0 throughput, 1 latency,
/// 2 overview, 3 box plot `index`, 4 RSSI. False for an unknown chart.
#[unsafe(no_mangle)]
pub extern "C" fn rst_chart(
    ptr: *mut RstCore,
    kind: u32,
    index: u32,
    w: f32,
    h: f32,
    label_h: f32,
    out: *mut RstScene,
) -> bool {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return false;
    };
    let Some(id) = chart_id(kind, index) else {
        return false;
    };
    // One build: the commands, points and text below all come from it. A
    // second call could rebuild the scene in between (new data arrived)
    // and pair commands with another scene's points.
    let (sc, generation) = c
        .core
        .scene_gen(id, w.max(1.0), h.max(1.0), label_h.max(1.0));
    let (plot, pts, text) = (sc.plot, sc.pts.as_ptr(), (sc.text.as_ptr(), sc.text.len()));
    let n_pts = sc.pts.len();
    let entry = c.cmds.entry(id).or_insert_with(|| (u64::MAX, Vec::new()));
    if entry.0 != generation {
        entry.1.clear();
        entry.1.extend(sc.prims.iter().map(encode));
        entry.0 = generation;
    }
    // SAFETY: Swift passes a pointer to a local RstScene.
    if let Some(out) = unsafe { out.as_mut() } {
        *out = RstScene {
            cmds: entry.1.as_ptr(),
            n_cmds: entry.1.len() as u32,
            pts: pts.cast(),
            n_pts: n_pts as u32,
            text: text.0,
            n_text: text.1 as u32,
            plot_x0: plot.x0,
            plot_y0: plot.y0,
            plot_x1: plot.x1,
            plot_y1: plot.y1,
            generation,
        };
    }
    true
}

fn encode(p: &Prim) -> RstCmd {
    match *p {
        Prim::Line {
            ref range,
            color,
            alpha,
            width,
            dashed,
        } => RstCmd {
            kind: 0,
            color: color as u8,
            flags: u8::from(dashed),
            alpha,
            a: width,
            start: range.start,
            end: range.end,
            ..RstCmd::default()
        },
        Prim::Area {
            ref range,
            base,
            color,
            alpha,
        } => RstCmd {
            kind: 1,
            color: color as u8,
            alpha,
            a: base,
            start: range.start,
            end: range.end,
            ..RstCmd::default()
        },
        Prim::Rect {
            rect,
            color,
            alpha,
            filled,
        } => RstCmd {
            kind: 2,
            color: color as u8,
            flags: u8::from(filled),
            alpha,
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
            ..RstCmd::default()
        },
        Prim::Dots {
            ref range,
            radius,
            color,
            alpha,
        } => RstCmd {
            kind: 3,
            color: color as u8,
            alpha,
            a: radius,
            start: range.start,
            end: range.end,
            ..RstCmd::default()
        },
        Prim::Text {
            at,
            ref range,
            size,
            color,
            h,
            v,
        } => RstCmd {
            kind: 4,
            color: color as u8,
            flags: size as u8 | (h as u8) << 2 | (v as u8) << 4,
            alpha: 1.0,
            start: range.start,
            end: range.end,
            x0: at.x,
            y0: at.y,
            ..RstCmd::default()
        },
    }
}

/// Hover over a chart at chart-local `(x, y)`, as JSON.
#[unsafe(no_mangle)]
pub extern "C" fn rst_hover(
    ptr: *mut RstCore,
    kind: u32,
    index: u32,
    w: f32,
    h: f32,
    x: f32,
    y: f32,
    out: *mut RstBytes,
) -> bool {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return false;
    };
    let Some(id) = chart_id(kind, index) else {
        return false;
    };
    let hv = c.core.hover(id, w, h, x, y);
    json::hover(&hv, &mut c.hover_json);
    bytes(&c.hover_json, out);
    true
}

/// Commands; see `include/rst.h`.
#[unsafe(no_mangle)]
pub extern "C" fn rst_command(ptr: *mut RstCore, cmd: u32) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    let core = &mut c.core;
    match cmd {
        0 => core.start(),
        1 => core.stop(),
        2 => core.pause(),
        3 => core.resume(),
        4 => core.reset(),
        5 => core.start_mode(RunMode::OneShot),
        6 => core.start_mode(RunMode::Continuous),
        7 => core.reset_view(),
        8 => core.set_mode(RunMode::OneShot),
        9 => core.set_mode(RunMode::Continuous),
        _ => return,
    }
    // Show the effect right away.
    c.wake.wake(Duration::ZERO);
}

/// Time-window gestures on the time-series charts: 0 zoom by `factor` at
/// `x`, 1 pan by `x` points, 2 center the overview on `x`. `w` is the
/// chart's width.
#[unsafe(no_mangle)]
pub extern "C" fn rst_view(ptr: *mut RstCore, op: u32, factor: f64, x: f32, w: f32) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    match op {
        0 => c.core.zoom(factor, x, w),
        1 => c.core.pan(x, w),
        2 => c.core.center(x, w),
        _ => {}
    }
}

/// While the time charts follow a live run: milliseconds until their time
/// axis has moved one point in a chart `w` wide. 0 when it isn't moving.
#[unsafe(no_mangle)]
pub extern "C" fn rst_live_ms(ptr: *mut RstCore, w: f32) -> u32 {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return 0;
    };
    c.core
        .live_interval(w.max(1.0))
        .map_or(0, |d| d.as_millis().clamp(1, 1000) as u32)
}

/// Choose option `index` of setting `setting` (`Setting` order).
#[unsafe(no_mangle)]
pub extern "C" fn rst_set(ptr: *mut RstCore, setting: u32, index: u32) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    if let Some(s) = u8::try_from(setting).ok().and_then(Setting::from_u8) {
        c.core.set(s, index as usize);
        c.wake.wake(Duration::ZERO);
    }
}

/// Rows `first..first + count` of log tab `tab`, newest first, as JSON
/// with the total row count.
#[unsafe(no_mangle)]
pub extern "C" fn rst_log(ptr: *mut RstCore, tab: u32, first: u32, count: u32, out: *mut RstBytes) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    let Some(tab) = log_tab(tab) else {
        return;
    };
    let total = c.core.log_len(tab);
    let rows = c.core.log_rows(tab, first as usize, count as usize);
    json::log_page(total, &rows, &mut c.log_json);
    bytes(&c.log_json, out);
}

/// Export every sample as JSON to the UTF-8 path; the result appears in the
/// status line.
#[unsafe(no_mangle)]
pub extern "C" fn rst_export(ptr: *mut RstCore, path: *const u8, len: usize) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(c) = (unsafe { core(ptr) }) else {
        return;
    };
    if path.is_null() {
        return;
    }
    // SAFETY: Swift passes a valid UTF-8 buffer of `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(path, len) };
    if let Ok(p) = std::str::from_utf8(bytes) {
        c.core.export(PathBuf::from(p));
    }
}

// Box plot ids are part of the ABI (chart kind 3's index).
const _: () = {
    assert!(BoxId::Download as u8 == 0 && BoxId::Udp as u8 == 4);
    assert!(std::mem::size_of::<RstCmd>() == 36);
};

#[cfg(test)]
mod tests {
    /// Every function in the header is exported here, and vice versa.
    #[test]
    fn header_matches_exports() {
        let header = include_str!("include/rst.h");
        let src = include_str!("ffi.rs");
        let mut exported: Vec<&str> = src
            .split("pub extern \"C\" fn ")
            .skip(1)
            .filter_map(|s| s.split('(').next())
            .collect();
        exported.sort();
        let mut declared: Vec<&str> = header
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.contains("typedef"))
            .filter_map(|l| l.split_once(" rst_").map(|(_, r)| r))
            .filter_map(|r| r.split('(').next())
            .filter(|n| *n != "ui_main")
            .collect();
        declared.sort();
        declared.dedup();
        let exported: Vec<String> = exported
            .iter()
            .map(|e| e.trim_start_matches("rst_").to_owned())
            .collect();
        assert_eq!(declared, exported);
    }
}
