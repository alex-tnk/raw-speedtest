//! macOS shell: a SwiftUI app compiled by build.rs into a static library and
//! linked into this binary. Rust keeps the process entry (so process setup
//! runs before AppKit exists); Swift calls back through the C ABI declared
//! in `include/rst.h`.

mod ffi;

unsafe extern "C" {
    /// Defined in `swift/App.swift`; runs the app and never returns. The
    /// core is opaque to Swift.
    fn rst_ui_main(core: *mut std::ffi::c_void) -> i32;
}

pub fn run() {
    let core = Box::into_raw(Box::new(ffi::RstCore::new()));
    // SAFETY: the core is leaked and lives for the rest of the process.
    let code = unsafe { rst_ui_main(core.cast()) };
    std::process::exit(code);
}
