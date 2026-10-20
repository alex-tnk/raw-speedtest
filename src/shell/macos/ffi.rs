//! The C ABI the SwiftUI shell calls. Every function is called on the main
//! thread except the callbacks Rust invokes. Returned buffers are borrowed
//! and stay valid until the next call of the same function.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[derive(Default)]
struct Shared {
    wake: Mutex<Option<Callback<WakeFn>>>,
    /// A wake is outstanding; cleared when the shell pulls data.
    pending: AtomicBool,
    points: Mutex<Vec<f32>>,
    version: AtomicU64,
}

impl Shared {
    fn wake(&self, delay_ms: u32) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let cb = *self.wake.lock().unwrap_or_else(|e| e.into_inner());
        match cb {
            Some(cb) => (cb.f)(cb.ctx, delay_ms),
            None => self.pending.store(false, Ordering::Release),
        }
    }
}

pub struct RstCore {
    shared: Arc<Shared>,
    /// Main-thread copy handed to Swift.
    snapshot: Vec<f32>,
}

impl RstCore {
    pub fn new() -> RstCore {
        let shared = Arc::new(Shared::default());
        let feed = shared.clone();
        std::thread::Builder::new()
            .name("spike-feed".into())
            .spawn(move || {
                let n = 10_000usize;
                let mut y = 0.5f32;
                let mut seed = 0x2545_f491_4f6c_dd1du64;
                let mut ys: std::collections::VecDeque<f32> = (0..n).map(|_| 0.5).collect();
                loop {
                    for _ in 0..40 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let r = (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
                        y = (y + r * 0.05).clamp(0.0, 1.0);
                        ys.pop_front();
                        ys.push_back(y);
                    }
                    {
                        let mut p = feed.points.lock().unwrap_or_else(|e| e.into_inner());
                        p.clear();
                        for (i, v) in ys.iter().enumerate() {
                            p.push(i as f32 / (n - 1) as f32);
                            p.push(*v);
                        }
                    }
                    feed.version.fetch_add(1, Ordering::AcqRel);
                    feed.wake(0);
                    std::thread::sleep(Duration::from_millis(33));
                }
            })
            .expect("spawn spike feed");
        RstCore {
            shared,
            snapshot: Vec::new(),
        }
    }
}

/// # Safety
/// `core` must come from `rst_ui_main`'s argument.
unsafe fn core<'a>(core: *mut RstCore) -> Option<&'a mut RstCore> {
    // SAFETY: per the function contract; main thread only.
    unsafe { core.as_mut() }
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_set_wake(core_ptr: *mut RstCore, f: Option<WakeFn>, ctx: *mut c_void) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(core) = (unsafe { core(core_ptr) }) else {
        return;
    };
    *core.shared.wake.lock().unwrap_or_else(|e| e.into_inner()) = f.map(|f| Callback { f, ctx });
    core.shared.pending.store(false, Ordering::Release);
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_set_quit(_core: *mut RstCore, f: Option<QuitFn>, ctx: *mut c_void) {
    if let Some(f) = f {
        let cb = Callback { f, ctx };
        crate::platform::set_quit_hook(Box::new(move || cb.quit()));
    }
}

/// Clears whatever AppKit stored in the app's preferences (save panel
/// folder, menu bar item visibility); the app keeps none.
#[unsafe(no_mangle)]
pub extern "C" fn rst_forget_preferences(_core: *mut RstCore) {
    crate::platform::current().forget_preferences();
}

#[repr(C)]
pub struct RstPoints {
    pub xy: *const f32,
    pub n: u32,
    pub version: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn rst_spike_points(core_ptr: *mut RstCore, out: *mut RstPoints) {
    // SAFETY: the pointer Swift received from `rst_ui_main`.
    let Some(core) = (unsafe { core(core_ptr) }) else {
        return;
    };
    core.shared.pending.store(false, Ordering::Release);
    {
        let p = core.shared.points.lock().unwrap_or_else(|e| e.into_inner());
        core.snapshot.clear();
        core.snapshot.extend_from_slice(&p);
    }
    let pts = RstPoints {
        xy: core.snapshot.as_ptr(),
        n: (core.snapshot.len() / 2) as u32,
        version: core.shared.version.load(Ordering::Acquire),
    };
    // SAFETY: Swift passes a pointer to a local RstPoints.
    if let Some(out) = unsafe { out.as_mut() } {
        *out = pts;
    }
}
