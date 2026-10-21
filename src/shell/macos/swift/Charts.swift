import AppKit
import RawSpeedCore
import SwiftUI

/// A chart scene from the core, turned into SwiftUI paths once per scene
/// generation.
final class SceneDraw {
    enum Op {
        case stroke(Path, color: Int, alpha: Double, width: CGFloat, dashed: Bool)
        case fill(Path, color: Int, alpha: Double)
        case text(Label, at: CGPoint, anchor: UnitPoint)
    }

    /// A piece of chart text. Drawn as a Canvas symbol, so SwiftUI keeps
    /// it typeset across redraws while only its position changes.
    struct Label: Hashable {
        let text: String
        let size: CGFloat
        let color: Int
    }

    let generation: UInt64
    let plot: CGRect
    let ops: [Op]
    /// Every distinct label in `ops`.
    let labels: [Label]

    @MainActor
    init(_ s: RstScene, model: Model) {
        generation = s.generation
        plot = CGRect(
            x: CGFloat(s.plot_x0), y: CGFloat(s.plot_y0),
            width: CGFloat(s.plot_x1 - s.plot_x0), height: CGFloat(s.plot_y1 - s.plot_y0))
        let cmds = UnsafeBufferPointer(start: s.cmds, count: Int(s.n_cmds))
        let pts = UnsafeBufferPointer(start: s.pts, count: Int(s.n_pts) * 2)
        let text = UnsafeBufferPointer(start: s.text, count: Int(s.n_text))
        func p(_ i: Int) -> CGPoint {
            CGPoint(x: CGFloat(pts[2 * i]), y: CGFloat(pts[2 * i + 1]))
        }
        var ops: [Op] = []
        var labels = Set<Label>()
        ops.reserveCapacity(cmds.count)
        for c in cmds {
            let (a, b) = (Int(c.start), Int(c.end))
            switch c.kind {
            case 0 where b - a >= 2:
                var path = Path()
                path.move(to: p(a))
                for i in (a + 1)..<b { path.addLine(to: p(i)) }
                ops.append(
                    .stroke(
                        path, color: Int(c.color), alpha: Double(c.alpha), width: CGFloat(c.a),
                        dashed: c.flags & 1 != 0))
            case 1 where b - a >= 2:
                let base = CGFloat(c.a)
                var path = Path()
                path.move(to: CGPoint(x: p(a).x, y: base))
                for i in a..<b { path.addLine(to: p(i)) }
                path.addLine(to: CGPoint(x: p(b - 1).x, y: base))
                path.closeSubpath()
                ops.append(.fill(path, color: Int(c.color), alpha: Double(c.alpha)))
            case 2:
                let r = CGRect(
                    x: CGFloat(c.x0), y: CGFloat(c.y0), width: CGFloat(c.x1 - c.x0),
                    height: CGFloat(c.y1 - c.y0))
                if c.flags & 1 != 0 {
                    ops.append(.fill(Path(r), color: Int(c.color), alpha: Double(c.alpha)))
                } else {
                    ops.append(
                        .stroke(
                            Path(r.insetBy(dx: 0.5, dy: 0.5)), color: Int(c.color),
                            alpha: Double(c.alpha), width: 1, dashed: false))
                }
            case 3:
                let r = CGFloat(c.a)
                var path = Path()
                for i in a..<b {
                    let q = p(i)
                    path.addEllipse(in: CGRect(x: q.x - r, y: q.y - r, width: 2 * r, height: 2 * r))
                }
                ops.append(.fill(path, color: Int(c.color), alpha: Double(c.alpha)))
            case 4:
                let s = String(decoding: UnsafeBufferPointer(rebasing: text[a..<b]), as: UTF8.self)
                let h = CGFloat((c.flags >> 2) & 3) / 2
                let v = CGFloat((c.flags >> 4) & 3) / 2
                let label = Label(
                    text: s, size: c.flags & 3 == 0 ? Theme.small : Theme.body,
                    color: Int(c.color))
                labels.insert(label)
                ops.append(
                    .text(
                        label, at: CGPoint(x: CGFloat(c.x0), y: CGFloat(c.y0)),
                        anchor: UnitPoint(x: h, y: v)))
            default:
                break
            }
        }
        self.ops = ops
        self.labels = Array(labels)
    }

    @MainActor
    func draw(_ ctx: inout GraphicsContext, model: Model) {
        let dash = (model.fixed?.dash ?? [4, 3]).map { CGFloat($0) }
        // The palette's colours adapt to the appearance; resolve each once
        // per draw rather than once per shape.
        let env = ctx.environment
        let colors = model.colors.map { Color($0.resolve(in: env)) }
        func color(_ token: Int) -> Color {
            colors.indices.contains(token) ? colors[token] : .primary
        }
        for op in ops {
            switch op {
            case .stroke(let path, let c, let a, let w, let dashed):
                ctx.stroke(
                    path, with: .color(color(c).opacity(a)),
                    style: StrokeStyle(lineWidth: w, lineJoin: .round, dash: dashed ? dash : []))
            case .fill(let path, let c, let a):
                ctx.fill(path, with: .color(color(c).opacity(a)))
            case .text(let label, let at, let anchor):
                if let sym = ctx.resolveSymbol(id: label) {
                    ctx.draw(sym, at: at, anchor: anchor)
                }
            }
        }
    }
}

/// A chart drawn by the core, with hover values and time-window gestures.
struct ChartView: View {
    let kind: ChartKind
    let height: CGFloat
    @Environment(Model.self) private var model
    // `State` directly: the `@State` macro needs a plugin only Xcode ships.
    private let hover = State<HoverV?>(initialValue: nil)
    private let size = State<CGSize>(initialValue: .zero)
    private let dragX = State<CGFloat>(initialValue: 0)
    private let magnify = State<CGFloat>(initialValue: 1)
    /// Where the pointer is, so the hover follows a scene that moves
    /// under a still pointer (a live chart scrolling).
    private let pointer = State<CGPoint?>(initialValue: nil)

    var body: some View {
        // Ask the core on every data update; redraw only when its scene for
        // this chart was rebuilt.
        let _ = model.dataVersion
        let _ = kind.isTime ? model.clock : 0
        let sz = size.wrappedValue
        let draw = sz.width > 0 && sz.height > 0 ? model.scene(kind, sz) : nil
        ZStack(alignment: .topLeading) {
            SceneCanvas(draw: draw, generation: draw?.generation ?? 0, model: model)
                .equatable()
            if let h = hover.wrappedValue {
                HoverMarks(hover: h, plot: model.plot(kind))
                if let tip = h.tip, !tip.isEmpty {
                    TipLayout(anchor: CGPoint(x: h.anchor.x, y: h.anchor.y)) {
                        TooltipView(tip: tip)
                    }
                    .allowsHitTesting(false)
                }
            }
        }
        .frame(height: height)
        .onGeometryChange(for: CGSize.self) { $0.size } action: { size.wrappedValue = $0 }
        .onChange(of: draw?.generation) {
            if let p = pointer.wrappedValue {
                hover.wrappedValue = model.hover(kind, size.wrappedValue, p)
            }
        }
        .contentShape(Rectangle())
        .onContinuousHover { phase in
            let s = size.wrappedValue
            switch phase {
            case .active(let p):
                model.hovered = (kind, s.width, p.x)
                pointer.wrappedValue = p
                hover.wrappedValue = model.hover(kind, s, p)
            case .ended:
                model.hovered = nil
                pointer.wrappedValue = nil
                hover.wrappedValue = nil
            }
        }
        .gesture(gestures(), including: kind.isTime ? .all : .subviews)
        .zIndex(hover.wrappedValue == nil ? 0 : 1)
    }

    private func gestures() -> some Gesture {
        let drag = DragGesture(minimumDistance: kind == .overview ? 0 : 3)
            .onChanged { v in
                let w = size.wrappedValue.width
                if kind == .overview {
                    model.center(v.location.x, width: w)
                } else {
                    let dx = v.translation.width - dragX.wrappedValue
                    dragX.wrappedValue = v.translation.width
                    model.pan(dx, width: w)
                }
            }
            .onEnded { _ in dragX.wrappedValue = 0 }
        let pinch = MagnifyGesture()
            .onChanged { v in
                let f = v.magnification / magnify.wrappedValue
                magnify.wrappedValue = v.magnification
                model.zoom(Double(f), at: v.startLocation.x, width: size.wrappedValue.width)
            }
            .onEnded { _ in magnify.wrappedValue = 1 }
        let reset = TapGesture(count: 2).onEnded { model.wholeRun() }
        return reset.exclusively(before: drag.simultaneously(with: pinch))
    }
}

/// Redraws only when the scene generation changes.
private struct SceneCanvas: View, Equatable {
    let draw: SceneDraw?
    let generation: UInt64
    let model: Model

    nonisolated static func == (a: SceneCanvas, b: SceneCanvas) -> Bool {
        a.generation == b.generation
    }

    var body: some View {
        Canvas { ctx, _ in
            draw?.draw(&ctx, model: model)
        } symbols: {
            ForEach(draw?.labels ?? [], id: \.self) { l in
                Text(l.text).font(.system(size: l.size)).monospacedDigit()
                    .foregroundStyle(model.color(l.color))
                    .fixedSize()
                    .tag(l)
            }
        }
    }
}

/// Crosshair and ringed point for the hovered sample.
private struct HoverMarks: View {
    let hover: HoverV
    let plot: CGRect
    @Environment(Model.self) private var model

    var body: some View {
        Canvas { ctx, _ in
            if let x = hover.crosshair {
                var line = Path()
                let x = CGFloat(x).rounded() + 0.5
                line.move(to: CGPoint(x: x, y: plot.minY))
                line.addLine(to: CGPoint(x: x, y: plot.maxY))
                ctx.stroke(line, with: .color(model.color(7)), lineWidth: 1)
            }
            if let h = hover.highlight {
                let r = CGFloat(model.fixed?.point_hover_radius ?? 3.5)
                let dot = Path(
                    ellipseIn: CGRect(x: h.x - r, y: h.y - r, width: 2 * r, height: 2 * r))
                ctx.fill(dot, with: .color(model.color(h.color)))
                ctx.stroke(dot, with: .color(model.color(9)), lineWidth: 1)
            }
        }
        .allowsHitTesting(false)
    }
}

/// Places one tooltip next to `anchor` without covering it: above and to
/// the right, flipped left at the right edge and below near the top.
struct TipLayout: Layout {
    let anchor: CGPoint

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        proposal.replacingUnspecifiedDimensions()
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) {
        guard let v = subviews.first else { return }
        var size = v.sizeThatFits(.unspecified)
        if size.width > Theme.tipMaxWidth {
            size = v.sizeThatFits(ProposedViewSize(width: Theme.tipMaxWidth, height: nil))
        }
        let gap = Theme.tipGap
        var x = bounds.minX + anchor.x + gap
        if x + size.width > bounds.maxX {
            x = bounds.minX + anchor.x - gap - size.width
        }
        var y = bounds.minY + anchor.y - gap - size.height
        if y < bounds.minY - size.height / 2 {
            y = bounds.minY + anchor.y + gap
        }
        v.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
    }
}

/// Exact values: a title, key/value pairs and how they're measured.
struct TooltipView: View {
    let tip: Tip

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            ForEach(Array(tip.enumerated()), id: \.offset) { _, row in
                if let t = row.title {
                    Text(t).font(.system(size: Theme.small, weight: .semibold))
                } else if let k = row.key {
                    HStack(alignment: .firstTextBaseline) {
                        Text(k).foregroundStyle(.secondary)
                        Spacer(minLength: Theme.s12)
                        Text(row.value ?? "").monospacedDigit()
                    }
                    .font(.system(size: Theme.small))
                } else if let n = row.note {
                    Text(n)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(Theme.s8)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: Theme.s8))
    }
}

/// Series samples above a chart.
struct LegendRow: View {
    let kind: ChartKind
    @Environment(Model.self) private var model

    var body: some View {
        let items = (kind == .latency ? model.legends?.latency : model.legends?.throughput) ?? []
        HStack(spacing: Theme.s12) {
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                HStack(spacing: Theme.s4) {
                    Rectangle()
                        .fill(model.color(item.color))
                        .frame(
                            width: item.tick ? 1.5 : Theme.legendSwatch,
                            height: item.tick ? Theme.s8 : 1.5)
                    Text(item.label)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}
