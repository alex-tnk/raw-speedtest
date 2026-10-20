import AppKit
import SwiftUI

struct SpikeRow: Identifiable {
    let id: Int
    var text: String { String(id) }
}

struct SpikeView: View {
    @Environment(Core.self) private var core

    var body: some View {
        @Bindable var core = core
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 24) {
                value("Download", size: 56)
                value("Upload", size: 56)
                value("Latency", size: 32)
            }
            Canvas { ctx, size in
                var path = Path()
                for (i, p) in core.points.enumerated() {
                    let pt = CGPoint(x: p.x * size.width, y: (1 - p.y) * size.height)
                    if i == 0 { path.move(to: pt) } else { path.addLine(to: pt) }
                }
                ctx.stroke(path, with: .color(.orange), lineWidth: 1.5)
            }
            .frame(height: 200)
            Table(core.rows) {
                TableColumn("Row") { Text($0.text).monospacedDigit() }
                TableColumn("Version") { _ in Text(String(core.version)).monospacedDigit() }
            }
        }
        .padding()
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Picker("Mode", selection: $core.mode) {
                    Text("One-shot").tag(0)
                    Text("Continuous").tag(1)
                }
                .pickerStyle(.segmented)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button("Start", systemImage: "play.fill") {}
                Button("Export…", systemImage: "square.and.arrow.up") { export() }
                Button("Inspector", systemImage: "sidebar.right") { core.inspector.toggle() }
            }
        }
        .inspector(isPresented: $core.inspector) {
            Form {
                Section("Server") {
                    LabeledContent("Data center", value: "—")
                    LabeledContent("Host", value: "speed.cloudflare.com")
                }
            }
        }
    }

    private func value(_ label: String, size: CGFloat) -> some View {
        VStack(alignment: .leading) {
            Text(label).font(.body)
            Text("—").font(.system(size: size)).monospacedDigit()
        }
    }

    private func export() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "raw-speed-test.json"
        panel.begin { _ in
            // The panel stores its last folder in the app's preferences.
            Core.shared.forgetPreferences()
        }
    }
}

struct SpikeSettings: View {
    @Environment(Core.self) private var core

    var body: some View {
        @Bindable var core = core
        Form {
            Toggle("Run in menu bar", isOn: $core.menuBar)
        }
        .padding()
        .frame(width: 360)
    }
}

struct SpikeMenu: View {
    @Environment(Core.self) private var core

    var body: some View {
        VStack(alignment: .leading) {
            Text("Version \(core.version)").monospacedDigit()
            Button("Quit Raw Speed Test") { NSApp.terminate(nil) }
        }
        .padding()
    }
}
