import AppKit
import SwiftUI

/// Every size, spacing and colour the macOS shell uses. Data colours come
/// from the core (light and dark values); neutrals are system colours.
enum Theme {
    // Type scale.
    static let hero: CGFloat = 56
    static let secondary: CGFloat = 32
    static let heroUnit: CGFloat = 15
    static let secondaryUnit: CGFloat = 13
    static let section: CGFloat = 15
    static let body: CGFloat = 13
    static let small: CGFloat = 11

    // Spacing (4 pt grid).
    static let s4: CGFloat = 4
    static let s8: CGFloat = 8
    static let s12: CGFloat = 12
    static let s16: CGFloat = 16

    // Layout.
    static let minWindow = CGSize(width: 1180, height: 760)
    /// Fits a 13-inch display at its default resolution.
    static let defaultWindow = CGSize(width: 1280, height: 820)
    static let throughputHeight: CGFloat = 196
    static let latencyHeight: CGFloat = 148
    static let overviewHeight: CGFloat = 32
    static let rssiHeight: CGFloat = 48
    static let boxRow: CGFloat = 24
    static let boxAxis: CGFloat = 20
    static let logHeight: CGFloat = 220
    static let swatch: CGFloat = 8
    static let legendSwatch: CGFloat = 12
    static let tipMaxWidth: CGFloat = 320
    static let tipGap: CGFloat = 12
    static let inspectorWidth: CGFloat = 300
    static let menuWidth: CGFloat = 260
    static let settingsWidth: CGFloat = 440
    /// Points per wheel "line" when a mouse reports lines.
    static let wheelLine: CGFloat = 24
    /// Log rows fetched from the core at a time.
    static let logPage = 200

    /// Height of an 11 pt text line, which keeps percentile labels apart.
    static let labelHeight: CGFloat = {
        let f = NSFont.systemFont(ofSize: small)
        return ceil(f.ascender - f.descender + f.leading)
    }()

    /// Colour tokens, in the core's order: download, upload, latency,
    /// jitter, loss, text, text2, text3, grid, background, accent,
    /// warning, error.
    static func colors(_ data: [ColorV]) -> [Color] {
        let system: [Int: NSColor] = [
            5: .labelColor,
            6: .secondaryLabelColor,
            7: .tertiaryLabelColor,
            8: .separatorColor,
            9: .windowBackgroundColor,
            10: .controlAccentColor,
        ]
        return data.enumerated().map { i, c in
            if let ns = system[i] { return Color(nsColor: ns) }
            guard let light = c.light, let dark = c.dark else { return .primary }
            return Color(nsColor: dynamic(light, dark))
        }
    }

    private static func dynamic(_ light: [Int], _ dark: [Int]) -> NSColor {
        func ns(_ c: [Int]) -> NSColor {
            NSColor(
                srgbRed: CGFloat(c[0]) / 255, green: CGFloat(c[1]) / 255,
                blue: CGFloat(c[2]) / 255, alpha: 1)
        }
        let (l, d) = (ns(light), ns(dark))
        return NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? d : l
        }
    }
}
