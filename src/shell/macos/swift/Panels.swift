import AppKit
import SwiftUI

/// Settings: continuous-mode load, IP version, and the menu bar.
struct SettingsView: View {
    @Environment(Model.self) private var model

    var body: some View {
        @Bindable var model = model
        Form {
            if let s = model.settings {
                Section {
                    ForEach(s.rows, id: \.id) { row in
                        Picker(
                            row.label,
                            selection: Binding(
                                get: { row.selected },
                                set: { model.set(row.id, $0) })
                        ) {
                            ForEach(Array(row.options.enumerated()), id: \.offset) { i, o in
                                Text(o).tag(i)
                            }
                        }
                    }
                    KvRow(kv: s.data_per_hour)
                }
            }
            Section {
                Toggle("Run in menu bar", isOn: $model.menuBar)
                    .help(
                        "Shows live values in the menu bar. Closing the window keeps the test running there."
                    )
            }
        }
        .formStyle(.grouped)
        .frame(width: Theme.settingsWidth)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// The menu bar item's window: live values and run controls.
struct MenuBarView: View {
    @Environment(Model.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let t = model.tray
        VStack(alignment: .leading, spacing: Theme.s8) {
            Grid(alignment: .leading, horizontalSpacing: Theme.s16, verticalSpacing: Theme.s4) {
                ForEach(Array((t?.rows ?? []).enumerated()), id: \.offset) { _, r in
                    GridRow {
                        Text(r.first ?? "").foregroundStyle(.secondary)
                        Text(r.count > 1 ? r[1] : "")
                            .monospacedDigit()
                            .gridColumnAlignment(.trailing)
                    }
                }
            }
            Text(t?.status ?? "")
                .font(.system(size: Theme.small))
                .foregroundStyle(.secondary)
                .monospacedDigit()
            Divider()
            MenuButton(title: "Run once", checked: t?.one_shot_running == true) {
                model.command(.runOnce)
                dismiss()
            }
            MenuButton(title: "Run continuously", checked: t?.continuous_running == true) {
                model.command(.runContinuously)
                dismiss()
            }
            let active = t?.one_shot_running == true || t?.continuous_running == true
            MenuButton(title: t?.paused == true ? "Resume" : "Pause") {
                model.command(t?.paused == true ? .resume : .pause)
                dismiss()
            }
            .disabled(!active)
            MenuButton(title: "Stop") {
                model.command(.stop)
                dismiss()
            }
            .disabled(!active)
            Divider()
            MenuButton(title: "Show window") {
                NSApp.setActivationPolicy(.regular)
                model.windowOpen = true
                openWindow(id: "main")
                NSApp.activate()
                dismiss()
            }
            MenuButton(title: "Quit Raw Speed Test") { NSApp.terminate(nil) }
        }
        .font(.system(size: Theme.body))
        .padding(Theme.s12)
        .frame(width: Theme.menuWidth)
    }
}

private struct MenuButton: View {
    let title: String
    var checked = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.s4) {
                Image(systemName: "checkmark").opacity(checked ? 1 : 0)
                Text(title)
                Spacer()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Icon plus the live latency, in the menu bar.
struct MenuBarLabel: View {
    @Environment(Model.self) private var model

    var body: some View {
        HStack(spacing: Theme.s4) {
            Image(systemName: "gauge.with.dots.needle.33percent")
            if let title = model.tray?.title, !title.isEmpty {
                Text(title).monospacedDigit()
            }
        }
    }
}
