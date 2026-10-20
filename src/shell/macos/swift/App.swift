import AppKit
import RawSpeedCore
import SwiftUI

/// Entry point, called from Rust's `main` after process setup.
@_cdecl("rst_ui_main")
public func rstUIMain(_ core: OpaquePointer?) -> Int32 {
    // Rust calls this on the main thread. The pointer crosses as an integer,
    // which is Sendable; Rust keeps the core alive for the whole process.
    let bits = UInt(bitPattern: core)
    MainActor.assumeIsolated {
        Core.shared.attach(OpaquePointer(bitPattern: bits))
        RSTApp.main()
    }
    return 0
}

/// The Rust core as seen from SwiftUI. Main actor only.
@MainActor
@Observable
final class Core {
    static let shared = Core()

    private var handle: OpaquePointer?
    private(set) var points: [CGPoint] = []
    private(set) var version: UInt64 = 0
    var menuBar = false {
        didSet {
            // AppKit records the status item's visibility in the app's
            // preferences once it is inserted or removed; clear it again.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                MainActor.assumeIsolated { Core.shared.forgetPreferences() }
            }
        }
    }
    var mode = 0
    var inspector = true
    let rows = (0..<20_000).map { SpikeRow(id: $0) }

    func attach(_ handle: OpaquePointer?) {
        self.handle = handle
        rst_set_wake(handle, coreWake, nil)
        rst_set_quit(handle, coreQuit, nil)
    }

    func forgetPreferences() {
        rst_forget_preferences(handle)
    }

    func pull() {
        var out = RstPoints()
        rst_spike_points(handle, &out)
        guard out.version != version, let xy = out.xy else { return }
        let buf = UnsafeBufferPointer(start: xy, count: Int(out.n) * 2)
        var pts = [CGPoint]()
        pts.reserveCapacity(Int(out.n))
        for i in 0..<Int(out.n) {
            pts.append(CGPoint(x: CGFloat(buf[2 * i]), y: CGFloat(buf[2 * i + 1])))
        }
        points = pts
        version = out.version
    }
}

// Rust calls these from its own threads. They are free functions, not
// closures in main-actor code, so Swift doesn't treat them as main-actor
// isolated (which would trap at runtime off the main thread).
private func coreWake(_ ctx: UnsafeMutableRawPointer?, _ delayMs: UInt32) {
    DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(delayMs))) {
        MainActor.assumeIsolated { Core.shared.pull() }
    }
}

private func coreQuit(_ ctx: UnsafeMutableRawPointer?) {
    DispatchQueue.main.async {
        MainActor.assumeIsolated { NSApp.terminate(nil) }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // A bare executable (outside the bundle) starts as a background app.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
        // AppKit records the (hidden) menu bar item's visibility at launch.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            MainActor.assumeIsolated { Core.shared.forgetPreferences() }
        }
        NotificationCenter.default.addObserver(
            self, selector: #selector(windowBecameKey(_:)),
            name: NSWindow.didBecomeKeyNotification, object: nil)
    }

    /// Windows keep no frame autosave and no restoration state.
    @objc private func windowBecameKey(_ note: Notification) {
        guard let window = note.object as? NSWindow else { return }
        window.isRestorable = false
        window.setFrameAutosaveName("")
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !Core.shared.menuBar
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }

    func application(_ app: NSApplication, shouldSaveSecureApplicationState coder: NSCoder) -> Bool {
        false
    }

    func application(_ app: NSApplication, shouldRestoreSecureApplicationState coder: NSCoder) -> Bool {
        false
    }
}

struct RSTApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    // No `@State`: in SDK 27 it is a macro whose plugin ships only with
    // Xcode. State lives in @Observable models (Observation's macros are in
    // the Command Line Tools).
    @Bindable private var core = Core.shared

    var body: some Scene {
        Window("Raw Speed Test", id: "main") {
            SpikeView()
                .environment(core)
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 1200, height: 800)

        Settings {
            SpikeSettings()
                .environment(core)
        }
        .restorationBehavior(.disabled)

        MenuBarExtra(isInserted: $core.menuBar) {
            SpikeMenu()
                .environment(core)
        } label: {
            Label("Raw Speed Test", systemImage: "gauge.with.dots.needle.33percent")
        }
        .menuBarExtraStyle(.window)
    }
}
