import AppKit
import RawSpeedCore
import SwiftUI

// MARK: - Values decoded from the core (src/shell/macos/json.rs)

struct TipRow: Decodable, Equatable {
    let title: String?
    let key: String?
    let value: String?
    let note: String?
}

typealias Tip = [TipRow]

struct HeadlineV: Decodable, Equatable {
    let label: String
    let value: String?
    let unit: String
    let color: Int
    let hero: Bool
    let tip: Tip
}

struct StatusV: Decodable, Equatable {
    let text: String
    let color: Int
}

struct ControlsV: Decodable, Equatable {
    let mode: Int
    let mode_enabled: Bool
    let active: Bool
    let paused: Bool
    let can_reset: Bool
    let can_export: Bool
}

struct KvV: Decodable, Equatable {
    let label: String
    let value: String?
    let tip: Tip
}

struct SettingRowV: Decodable, Equatable {
    let id: Int
    let label: String
    let options: [String]
    let selected: Int
}

struct SettingsV: Decodable, Equatable {
    let rows: [SettingRowV]
    let data_per_hour: KvV
}

struct ItemV: Decodable, Equatable {
    let sub: String?
    let boxes: Int?
    let rows: Int?
    let kv: KvV?
}

struct ColumnV: Decodable, Equatable {
    let title: String
    let items: [ItemV]
}

struct SectionV: Decodable, Equatable {
    let title: String
    let rows: [KvV]
}

struct TrayV: Decodable, Equatable {
    let title: String
    let rows: [[String]]
    let status: String
    let one_shot_running: Bool
    let continuous_running: Bool
    let paused: Bool
}

struct LegendV: Decodable, Equatable {
    let label: String
    let color: Int
    let tick: Bool
}

struct LegendsV: Decodable, Equatable {
    let throughput: [LegendV]
    let latency: [LegendV]
}

struct FrameV: Decodable {
    let headline: [HeadlineV]
    let status: StatusV
    let progress: Double?
    let elapsed: String?
    let controls: ControlsV
    let settings: SettingsV
    let columns: [ColumnV]
    let info: [SectionV]
    let rssi: Bool
    let log_tabs: [String]
    let log_counts: [Int]
    let tray: TrayV
    let legends: LegendsV
    let export_name: String
    let next_tick_ms: Int?
}

struct PointV: Decodable, Equatable {
    let x: Double
    let y: Double
}

struct HighlightV: Decodable, Equatable {
    let x: Double
    let y: Double
    let color: Int
}

struct HoverV: Decodable, Equatable {
    let crosshair: Double?
    let highlight: HighlightV?
    let tip: Tip?
    let anchor: PointV
}

struct CellV: Decodable {
    let text: String
    let color: Int
}

struct LogRowV: Decodable {
    let id: UInt64
    let cells: [CellV]
    let tip: Tip?
}

struct LogPageV: Decodable {
    let total: Int
    let rows: [LogRowV]
}

struct LogColumnV: Decodable {
    let title: String
    let numeric: Bool
    let width: Double
}

struct ColorV: Decodable {
    let light: [Int]?
    let dark: [Int]?
}

struct FixedV: Decodable {
    let colors: [ColorV]
    let columns: [[LogColumnV]]
    let dash: [Double]
    let point_hover_radius: Double
}

/// A chart the core draws.
enum ChartKind: Hashable {
    case throughput, latency, overview, rssi
    case box(Int)

    var raw: (UInt32, UInt32) {
        switch self {
        case .throughput: (0, 0)
        case .latency: (1, 0)
        case .overview: (2, 0)
        case .box(let i): (3, UInt32(i))
        case .rssi: (4, 0)
        }
    }

    /// Shares the time window (zoom, pan) with the other time charts.
    var isTime: Bool {
        switch self {
        case .throughput, .latency, .overview: true
        default: false
        }
    }
}

enum Command: UInt32 {
    case start = 0, stop, pause, resume, reset, runOnce, runContinuously, wholeRun, oneShotMode,
        continuousMode
}

/// One element of a list in the frame, observed on its own, so a change
/// to one breakdown column or headline value updates only its view.
@MainActor
@Observable
final class Cell<T: Equatable>: Identifiable {
    let id: Int
    var value: T

    init(_ id: Int, _ value: T) {
        self.id = id
        self.value = value
    }
}

extension Array {
    /// Updates `cells` in place from `values`, keeping cell identity when
    /// the count is unchanged.
    @MainActor
    static func sync<T: Equatable>(_ cells: inout [Cell<T>], _ values: [T]) where Element == Cell<T> {
        if cells.count != values.count {
            cells = values.enumerated().map { Cell($0, $1) }
            return
        }
        for (c, v) in zip(cells, values) where c.value != v {
            c.value = v
        }
    }
}

// MARK: - The core, as seen from SwiftUI

/// Main actor only. Holds the latest frame and turns user actions into
/// calls into the core.
@MainActor
@Observable
final class Model {
    static let shared = Model()

    @ObservationIgnored private var handle: OpaquePointer?
    // The frame in parts, each assigned only when it changed, so a view
    // updates only when its own data does.
    private(set) var loaded = false
    private(set) var headline: [Cell<HeadlineV>] = []
    private(set) var status = StatusV(text: "", color: 6)
    private(set) var progress: Double?
    private(set) var elapsed: String?
    private(set) var controls: ControlsV?
    private(set) var settings: SettingsV?
    private(set) var columns: [Cell<ColumnV>] = []
    private(set) var info: [SectionV] = []
    private(set) var rssi = false
    private(set) var logTabs: [String] = []
    private(set) var logCounts: [Int] = []
    private(set) var tray: TrayV?
    private(set) var legends: LegendsV?
    @ObservationIgnored private(set) var exportName = ""
    private(set) var fixed: FixedV?
    /// Bumped on every pull, so charts ask for their scene again (the time
    /// axis moves with the clock even without new data).
    private(set) var dataVersion: UInt64 = 0
    var inspector = true
    var logTab = 0
    var menuBar = false {
        didSet {
            // AppKit records the status item's visibility in the app's
            // preferences once it is inserted or removed; clear it again.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                MainActor.assumeIsolated { Model.shared.forgetPreferences() }
            }
        }
    }

    @ObservationIgnored private var lastFrame = Data()
    @ObservationIgnored private var tick: DispatchWorkItem?
    @ObservationIgnored private var scenes: [ChartKind: SceneDraw] = [:]
    @ObservationIgnored private var pages: [Int: [LogRowV]] = [:]
    @ObservationIgnored private var pageCounts: [Int] = [-1, -1, -1]
    @ObservationIgnored private(set) var colors: [Color] = []
    @ObservationIgnored private let decoder = JSONDecoder()
    /// The chart under the pointer, for scroll-wheel zoom and pan.
    @ObservationIgnored var hovered: (kind: ChartKind, width: CGFloat, x: CGFloat)?

    func attach(_ handle: OpaquePointer?) {
        self.handle = handle
        rst_set_wake(handle, coreWake, nil)
        rst_set_quit(handle, coreQuit, nil)
        var b = RstBytes()
        rst_fixed(handle, &b)
        if let f = try? decoder.decode(FixedV.self, from: Self.data(b)) {
            fixed = f
            colors = Theme.colors(f.colors)
        }
        pull()
    }

    private static func data(_ b: RstBytes) -> Data {
        guard let p = b.ptr, b.len > 0 else { return Data() }
        return Data(bytes: p, count: b.len)
    }

    /// Fetch the frame; called on a wake, a tick, or after a command.
    func pull() {
        var b = RstBytes()
        rst_frame(handle, &b)
        let data = Self.data(b)
        if data != lastFrame {
            lastFrame = data
            if let f = try? decoder.decode(FrameV.self, from: data) {
                if f.log_counts != pageCounts {
                    pages.removeAll()
                    pageCounts = f.log_counts
                }
                apply(f)
            }
        }
        if windowOpen { dataVersion &+= 1 }
        tick?.cancel()
        if let ms = nextTick {
            let item = DispatchWorkItem {
                MainActor.assumeIsolated { Model.shared.pull() }
            }
            tick = item
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(ms), execute: item)
        }
    }

    @ObservationIgnored private var nextTick: Int?
    @ObservationIgnored private var lastDecoded: FrameV?

    /// While the main window is closed (menu bar mode) only the menu bar
    /// values update; SwiftUI would otherwise keep rendering the closed
    /// window's content.
    @ObservationIgnored var windowOpen = true {
        didSet {
            if windowOpen && !oldValue, let f = lastDecoded {
                apply(f)
                dataVersion &+= 1
            }
        }
    }

    private func apply(_ f: FrameV) {
        lastDecoded = f
        exportName = f.export_name
        nextTick = f.next_tick_ms
        if tray != f.tray { tray = f.tray }
        guard windowOpen else { return }
        func set<T: Equatable>(_ kp: ReferenceWritableKeyPath<Model, T>, _ v: T) {
            if self[keyPath: kp] != v { self[keyPath: kp] = v }
        }
        Array.sync(&headline, f.headline)
        set(\.status, f.status)
        set(\.progress, f.progress)
        set(\.elapsed, f.elapsed)
        set(\.controls, f.controls)
        set(\.settings, f.settings)
        Array.sync(&columns, f.columns)
        set(\.info, f.info)
        set(\.rssi, f.rssi)
        set(\.logTabs, f.log_tabs)
        set(\.logCounts, f.log_counts)
        set(\.legends, f.legends)
        set(\.loaded, true)
    }

    func color(_ token: Int) -> Color {
        colors.indices.contains(token) ? colors[token] : .primary
    }

    func command(_ c: Command) {
        rst_command(handle, c.rawValue)
    }

    func set(_ setting: Int, _ index: Int) {
        rst_set(handle, UInt32(setting), UInt32(index))
    }

    func forgetPreferences() {
        rst_forget_preferences(handle)
    }

    // MARK: Charts

    /// Drawing for chart `kind` at `size`, rebuilt only when the core's
    /// scene changed.
    func scene(_ kind: ChartKind, _ size: CGSize) -> SceneDraw? {
        var s = RstScene()
        let (k, i) = kind.raw
        guard rst_chart(handle, k, i, Float(size.width), Float(size.height), Float(Theme.labelHeight), &s)
        else { return nil }
        if let cached = scenes[kind], cached.generation == s.generation {
            return cached
        }
        let draw = SceneDraw(s, model: self)
        scenes[kind] = draw
        return draw
    }

    /// The plot area of the chart's last scene.
    func plot(_ kind: ChartKind) -> CGRect {
        scenes[kind]?.plot ?? .zero
    }

    func hover(_ kind: ChartKind, _ size: CGSize, _ p: CGPoint) -> HoverV? {
        var b = RstBytes()
        let (k, i) = kind.raw
        guard
            rst_hover(
                handle, k, i, Float(size.width), Float(size.height), Float(p.x), Float(p.y), &b)
        else { return nil }
        return try? decoder.decode(HoverV.self, from: Self.data(b))
    }

    func zoom(_ factor: Double, at x: CGFloat, width: CGFloat) {
        rst_view(handle, 0, factor, Float(x), Float(width))
        dataVersion &+= 1
    }

    func pan(_ dx: CGFloat, width: CGFloat) {
        rst_view(handle, 1, 1, Float(dx), Float(width))
        dataVersion &+= 1
    }

    func center(_ x: CGFloat, width: CGFloat) {
        rst_view(handle, 2, 1, Float(x), Float(width))
        dataVersion &+= 1
    }

    func wholeRun() {
        command(.wholeRun)
        dataVersion &+= 1
    }

    /// Scroll wheel over a time chart: Command or Control zooms, a
    /// horizontal scroll pans. Returns false to let the page scroll.
    func scroll(_ event: NSEvent) -> Bool {
        guard let h = hovered, h.kind.isTime else { return false }
        let k: CGFloat = event.hasPreciseScrollingDeltas ? 1 : Theme.wheelLine
        if !event.modifierFlags.intersection([.command, .control]).isEmpty {
            let dy = event.scrollingDeltaY * k
            if dy != 0 { zoom(exp(Double(dy) / 200), at: h.x, width: h.width) }
            return true
        }
        let dx = event.scrollingDeltaX * k
        if abs(dx) > abs(event.scrollingDeltaY) {
            pan(dx, width: h.width)
            return true
        }
        return false
    }

    // MARK: Log

    func logRow(_ tab: Int, _ index: Int) -> LogRowV? {
        let page = index / Theme.logPage
        let key = tab * 1_000_000 + page
        if pages[key] == nil {
            var b = RstBytes()
            rst_log(handle, UInt32(tab), UInt32(page * Theme.logPage), UInt32(Theme.logPage), &b)
            pages[key] = (try? decoder.decode(LogPageV.self, from: Self.data(b)))?.rows ?? []
        }
        let rows = pages[key] ?? []
        let j = index - page * Theme.logPage
        return rows.indices.contains(j) ? rows[j] : nil
    }

    // MARK: Export

    func export() {
        let name = exportName
        guard !name.isEmpty else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.canCreateDirectories = true
        panel.begin { response in
            MainActor.assumeIsolated {
                // The panel stores its last folder in the app's preferences.
                Model.shared.forgetPreferences()
                guard response == .OK, let url = panel.url else { return }
                Model.shared.exportTo(url.path)
            }
        }
    }

    private func exportTo(_ path: String) {
        var bytes = Array(path.utf8)
        rst_export(handle, &bytes, bytes.count)
    }
}

// Rust calls these from its own threads. They are free functions, not
// closures in main-actor code, so Swift doesn't treat them as main-actor
// isolated (which would trap at runtime off the main thread).
private func coreWake(_ ctx: UnsafeMutableRawPointer?, _ delayMs: UInt32) {
    DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(delayMs))) {
        MainActor.assumeIsolated { Model.shared.pull() }
    }
}

private func coreQuit(_ ctx: UnsafeMutableRawPointer?) {
    DispatchQueue.main.async {
        MainActor.assumeIsolated { NSApp.terminate(nil) }
    }
}

/// Tooltip rows as one string, for native `.help` tooltips.
func tipText(_ tip: Tip) -> String {
    tip.map { row in
        if let t = row.title { return t }
        if let k = row.key { return "\(k): \(row.value ?? "")" }
        return row.note ?? ""
    }.joined(separator: "\n")
}
