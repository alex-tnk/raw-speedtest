import AppKit
import SwiftUI

/// The main window: headline values, live charts, status, breakdowns and
/// the log, with run controls in the toolbar and connection info in the
/// inspector.
struct MainView: View {
    @Environment(Model.self) private var model

    var body: some View {
        @Bindable var model = model
        ScrollView {
            if model.loaded {
                VStack(alignment: .leading, spacing: Theme.s12) {
                    HeadlineRow()
                    LegendRow(kind: .throughput)
                    ChartView(kind: .throughput, height: Theme.throughputHeight)
                    LegendRow(kind: .latency)
                    ChartView(kind: .latency, height: Theme.latencyHeight)
                    ChartView(kind: .overview, height: Theme.overviewHeight)
                    StatusRow()
                    BreakdownRow()
                    LogPanel()
                }
                .padding(Theme.s16)
            }
        }
        // The minimum window size includes the inspector column.
        .frame(
            minWidth: Theme.minWindow.width - Theme.inspectorWidth,
            minHeight: Theme.minWindow.height)
        .toolbar { Toolbar() }
        .inspector(isPresented: $model.inspector) {
            InfoPanel()
                .inspectorColumnWidth(
                    min: Theme.inspectorWidth - 40, ideal: Theme.inspectorWidth,
                    max: Theme.inspectorWidth + 100)
        }
    }
}

/// Run controls: mode, start/stop, pause/resume, reset, export, settings.
private struct Toolbar: ToolbarContent {
    @Environment(Model.self) private var model

    var body: some ToolbarContent {
        let c = model.controls
        ToolbarItem(placement: .navigation) {
            Picker(
                "Mode",
                selection: Binding(
                    get: { c?.mode ?? 0 },
                    set: { model.command($0 == 0 ? .oneShotMode : .continuousMode) })
            ) {
                Text("One-shot").tag(0)
                Text("Continuous").tag(1)
            }
            .pickerStyle(.segmented)
            .disabled(!(c?.mode_enabled ?? true))
            .help("One-shot runs the full test once; continuous repeats it until stopped.")
        }
        ToolbarItemGroup(placement: .primaryAction) {
            if c?.active == true {
                Button("Stop", systemImage: "stop.fill") { model.command(.stop) }
                    .help("Stop")
                if c?.paused == true {
                    Button("Resume", systemImage: "play") { model.command(.resume) }
                        .help("Resume")
                } else {
                    Button("Pause", systemImage: "pause") { model.command(.pause) }
                        .help("Pause")
                }
            } else {
                Button("Start", systemImage: "play.fill") { model.command(.start) }
                    .help("Start")
                Button("Pause", systemImage: "pause") {}
                    .disabled(true)
            }
            Button("Reset", systemImage: "arrow.counterclockwise") { model.command(.reset) }
                .disabled(!(c?.can_reset ?? false))
                .help("Reset")
            Button("Export…", systemImage: "square.and.arrow.up") { model.export() }
                .disabled(!(c?.can_export ?? false))
                .help("Export…")
            SettingsLink {
                Label("Settings", systemImage: "gearshape")
            }
            .help("Settings")
            Button("Connection", systemImage: "sidebar.right") { model.inspector.toggle() }
                .help("Connection")
        }
    }
}

/// Download, upload, latency, jitter and packet loss.
private struct HeadlineRow: View {
    @Environment(Model.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: Theme.s16) {
            ForEach(model.headline) { cell in
                HeadlineBlock(cell: cell)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

private struct HeadlineBlock: View {
    let cell: Cell<HeadlineV>
    @Environment(Model.self) private var model
    private let hovering = State(initialValue: false)

    var body: some View {
        let h = cell.value
        VStack(alignment: .leading, spacing: Theme.s4) {
            HStack(spacing: Theme.s4) {
                Circle()
                    .fill(model.color(h.color))
                    .frame(width: Theme.swatch, height: Theme.swatch)
                Text(h.label).font(.system(size: Theme.body))
            }
            HStack(alignment: .firstTextBaseline, spacing: Theme.s4) {
                Text(h.value ?? "—")
                    .font(.system(size: h.hero ? Theme.hero : Theme.secondary))
                    .monospacedDigit()
                    .foregroundStyle(h.value == nil ? .secondary : .primary)
                if h.value != nil {
                    Text(h.unit)
                        .font(.system(size: h.hero ? Theme.heroUnit : Theme.secondaryUnit))
                        .foregroundStyle(.secondary)
                }
            }
            .lineLimit(1)
        }
        .onHover { hovering.wrappedValue = $0 }
        .overlay(alignment: .bottomLeading) {
            if hovering.wrappedValue, h.value != nil {
                TooltipView(tip: h.tip)
                    .frame(maxWidth: Theme.tipMaxWidth, alignment: .leading)
                    .fixedSize()
                    .alignmentGuide(.bottom) { d in d[.top] - Theme.s4 }
                    .allowsHitTesting(false)
            }
        }
        .zIndex(hovering.wrappedValue ? 1 : 0)
    }
}

/// Status line, elapsed time and the progress line.
private struct StatusRow: View {
    @Environment(Model.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            HStack(alignment: .firstTextBaseline) {
                Text(model.status.text)
                    .foregroundStyle(model.color(model.status.color))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .monospacedDigit()
                Spacer()
                if let e = model.elapsed {
                    Text("Elapsed").foregroundStyle(.secondary)
                    Text(e).monospacedDigit()
                }
            }
            .font(.system(size: Theme.body))
            ProgressView(value: model.progress ?? 0)
                .progressViewStyle(.linear)
                .opacity(model.progress == nil ? 0.3 : 1)
        }
    }
}

/// Throughput by payload size, latency by load, packet loss.
private struct BreakdownRow: View {
    @Environment(Model.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: Theme.s12) {
            ForEach(model.columns) { cell in
                ColumnView(cell: cell)
            }
        }
    }
}

private struct ColumnView: View {
    let cell: Cell<ColumnV>

    var body: some View {
        let col = cell.value
        GroupBox {
            VStack(alignment: .leading, spacing: Theme.s8) {
                Text(col.title).font(.system(size: Theme.section, weight: .semibold))
                ForEach(Array(col.items.enumerated()), id: \.offset) { _, item in
                    ItemView(item: item)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct ItemView: View {
    let item: ItemV

    var body: some View {
        if let s = item.sub {
            Text(s).font(.system(size: Theme.small)).foregroundStyle(.secondary)
                .padding(.top, Theme.s4)
        } else if let b = item.boxes {
            ChartView(
                kind: .box(b),
                height: CGFloat(max(item.rows ?? 1, 1)) * Theme.boxRow + Theme.boxAxis)
        } else if let kv = item.kv {
            KvRow(kv: kv)
        }
    }
}

/// A label with its value right-aligned; hover explains the value.
struct KvRow: View {
    let kv: KvV

    var body: some View {
        // Values never truncate: a value too wide for the space beside its
        // label wraps onto further right-aligned lines.
        HStack(alignment: .firstTextBaseline) {
            Text(kv.label).foregroundStyle(.secondary).fixedSize()
            Spacer(minLength: Theme.s8)
            Text(kv.value ?? "—")
                .monospacedDigit()
                .foregroundStyle(kv.value == nil ? .secondary : .primary)
                .multilineTextAlignment(.trailing)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .font(.system(size: Theme.body))
        .help(tipText(kv.tip))
    }
}

/// Requests, connections and errors, newest first.
private struct LogPanel: View {
    @Environment(Model.self) private var model

    var body: some View {
        @Bindable var model = model
        GroupBox {
            VStack(alignment: .leading, spacing: Theme.s8) {
                Picker("Log", selection: $model.logTab) {
                    ForEach(0..<3, id: \.self) { i in
                        Text(model.logTabs.indices.contains(i) ? model.logTabs[i] : "").tag(i)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                LogTable(
                    tab: model.logTab,
                    count: model.logCounts.indices.contains(model.logTab)
                        ? model.logCounts[model.logTab] : 0)
                    .frame(height: Theme.logHeight)
            }
        }
    }
}

private struct LogRef: Identifiable {
    let id: Int
    let index: Int
}

private struct LogTable: View {
    let tab: Int
    let count: Int
    @Environment(Model.self) private var model

    var body: some View {
        let columns = model.fixed?.columns[tab] ?? []
        // Ids count from the oldest row, so they stay put as rows arrive.
        let refs = (0..<count).map { LogRef(id: count - 1 - $0, index: $0) }
        Table(refs) {
            TableColumnForEach(Array(columns.enumerated()), id: \.offset) { ci, col in
                TableColumn(col.title) { (ref: LogRef) in
                    LogCell(tab: tab, index: ref.index, column: ci, numeric: col.numeric)
                }
                .width(ideal: CGFloat(col.width))
            }
        }
        .id(tab)
    }
}

private struct LogCell: View {
    let tab: Int
    let index: Int
    let column: Int
    let numeric: Bool
    @Environment(Model.self) private var model

    var body: some View {
        let row = model.logRow(tab, index)
        let cell = row.flatMap { $0.cells.indices.contains(column) ? $0.cells[column] : nil }
        Text(cell?.text ?? "")
            .monospacedDigit()
            .foregroundStyle(model.color(cell?.color ?? 5))
            .frame(maxWidth: .infinity, alignment: numeric ? .trailing : .leading)
            .help(row?.tip.map(tipText) ?? "")
    }
}

/// Connection, server and Wi-Fi details.
private struct InfoPanel: View {
    @Environment(Model.self) private var model

    var body: some View {
        Form {
            ForEach(Array(model.info.enumerated()), id: \.offset) { _, section in
                Section(section.title) {
                    ForEach(Array(section.rows.enumerated()), id: \.offset) { _, row in
                        KvRow(kv: row)
                    }
                    if section.title == "Wi-Fi", model.rssi {
                        ChartView(kind: .rssi, height: Theme.rssiHeight)
                    }
                }
            }
        }
        .formStyle(.grouped)
    }
}
