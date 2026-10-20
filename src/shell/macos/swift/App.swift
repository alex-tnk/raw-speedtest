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
        Model.shared.attach(OpaquePointer(bitPattern: bits))
        RSTApp.main()
    }
    return 0
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var scrollMonitor: Any?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // A bare executable (outside the bundle) starts as a background app.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
        let center = NotificationCenter.default
        center.addObserver(
            self, selector: #selector(windowBecameKey(_:)),
            name: NSWindow.didBecomeKeyNotification, object: nil)
        center.addObserver(
            self, selector: #selector(windowWillClose(_:)),
            name: NSWindow.willCloseNotification, object: nil)
        // Scroll wheel over a time chart zooms (with Command or Control) or
        // pans (horizontally); everything else scrolls the page.
        // Local monitors run on the main thread; the closure inherits this
        // method's main-actor isolation.
        scrollMonitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
            Model.shared.scroll(event) ? nil : event
        }
        // AppKit records the (hidden) menu bar item's visibility at launch.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            MainActor.assumeIsolated { Model.shared.forgetPreferences() }
        }
    }

    /// Windows keep no frame autosave and no restoration state.
    @objc private func windowBecameKey(_ note: Notification) {
        guard let window = note.object as? NSWindow else { return }
        window.isRestorable = false
        window.setFrameAutosaveName("")
        if Self.isMain(window) { Model.shared.windowOpen = true }
    }

    private static func isMain(_ window: NSWindow) -> Bool {
        window.identifier?.rawValue.hasPrefix("main") == true
    }

    /// Closing the main window stops its updates; in menu bar mode the app
    /// keeps running without a Dock icon.
    @objc private func windowWillClose(_ note: Notification) {
        guard let window = note.object as? NSWindow, Self.isMain(window) else { return }
        Model.shared.windowOpen = false
        guard Model.shared.menuBar else { return }
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if !NSApp.windows.contains(where: {
                    $0.isVisible && $0.identifier?.rawValue.hasPrefix("main") == true
                }) {
                    NSApp.setActivationPolicy(.accessory)
                }
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !Model.shared.menuBar
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }

    func application(_ app: NSApplication, shouldSaveSecureApplicationState coder: NSCoder) -> Bool {
        false
    }

    func application(_ app: NSApplication, shouldRestoreSecureApplicationState coder: NSCoder)
        -> Bool
    {
        false
    }
}

struct RSTApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    // No `@State`: in SDK 27 it is a macro whose plugin ships only with
    // Xcode. State lives in @Observable models (Observation's macros are in
    // the Command Line Tools).
    @Bindable private var model = Model.shared

    var body: some Scene {
        Window("Raw Speed Test", id: "main") {
            MainView()
                .environment(model)
        }
        .restorationBehavior(.disabled)
        .defaultSize(Theme.defaultWindow)

        Settings {
            SettingsView()
                .environment(model)
        }
        .restorationBehavior(.disabled)

        MenuBarExtra(isInserted: $model.menuBar) {
            MenuBarView()
                .environment(model)
        } label: {
            MenuBarLabel()
                .environment(model)
        }
        .menuBarExtraStyle(.window)
    }
}
