//! Colour tokens and chart metrics. Data colours have fixed light and dark
//! values, checked against each platform's backgrounds below; neutrals map
//! to the platform's own semantic colours. No colour or chart size is
//! written anywhere else.

/// What shells paint with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Color {
    Download = 0,
    Upload = 1,
    Latency = 2,
    Jitter = 3,
    Loss = 4,
    /// Primary label colour: values and labels.
    Text = 5,
    /// Secondary label colour: units, axis labels, the gateway line.
    Text2 = 6,
    /// Tertiary label colour: the crosshair, disabled controls.
    Text3 = 7,
    /// Gridlines and separators.
    Grid = 8,
    /// Content background (dims the overview outside the view).
    Background = 9,
    /// The platform's accent colour.
    Accent = 10,
    Warning = 11,
    Error = 12,
}

/// Gateway latency is drawn as a neutral reference line.
pub const GATEWAY: Color = Color::Text2;

impl Color {
    /// Data colours as sRGB `[light, dark]`; `None` for neutrals, which
    /// come from the platform.
    pub fn rgb(self) -> Option<[[u8; 3]; 2]> {
        Some(match self {
            Color::Download => [hex(0xEA580C), hex(0xFF9500)],
            Color::Upload => [hex(0x0E7490), hex(0x2EE0D6)],
            Color::Latency => [hex(0x2563EB), hex(0x7C9CFF)],
            Color::Jitter => [hex(0xA16207), hex(0xFFE14D)],
            Color::Loss | Color::Error => [hex(0xBE123C), hex(0xFF6B6B)],
            Color::Warning => [hex(0xA14600), hex(0xFFB547)],
            _ => return None,
        })
    }

    /// Every token, in `repr` order.
    // Not every shell uses every part of the view layer.
    #[allow(dead_code)]
    pub const ALL: [Color; 13] = [
        Color::Download,
        Color::Upload,
        Color::Latency,
        Color::Jitter,
        Color::Loss,
        Color::Text,
        Color::Text2,
        Color::Text3,
        Color::Grid,
        Color::Background,
        Color::Accent,
        Color::Warning,
        Color::Error,
    ];
}

const fn hex(rgb: u32) -> [u8; 3] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

// Opacities.
/// Flat fill under a data line and box plot boxes.
pub const FILL_ALPHA: f32 = 0.12;
/// Throughput area in the overview strip.
pub const OVERVIEW_FILL_ALPHA: f32 = 0.35;
/// Min–max bars of aggregated buckets.
pub const ENVELOPE_ALPHA: f32 = 0.5;
/// Dimming of the overview strip outside the visible window.
pub const OUTSIDE_VIEW_ALPHA: f32 = 0.6;
/// Load and pause bands behind the latency chart.
pub const BAND_ALPHA: f32 = 0.07;
pub const PERCENTILE_ALPHA: f32 = 0.7;

// Chart geometry, in points.
pub const S4: f32 = 4.0;
pub const S8: f32 = 8.0;
pub const S16: f32 = 16.0;
pub const STROKE: f32 = 1.0;
pub const DATA_LINE: f32 = 1.5;
pub const POINT_RADIUS: f32 = 2.0;
pub const POINT_HOVER_RADIUS: f32 = 3.5;
/// Dash pattern of percentile lines (on, off).
pub const DASH: [f32; 2] = [4.0, 3.0];
/// Pointer distance that counts as hovering a point.
pub const HOVER_PX: f32 = 6.0;
/// Farthest a crosshair snaps to a sample.
pub const SNAP_PX: f32 = 40.0;
pub const LOSS_TICK: f32 = 8.0;
/// Lane under the latency plot that holds the loss ticks, so they never
/// sit on the data.
pub const LOSS_LANE: f32 = LOSS_TICK + S4;
/// Width of one column when dense samples are drawn as a per-column
/// range band and median line.
pub const COLUMN: f32 = 2.0;
/// Full-range and interquartile bands of dense samples.
pub const RANGE_ALPHA: f32 = 0.12;
pub const IQR_ALPHA: f32 = 0.3;
/// Below this average width, requests are drawn as one point each (their
/// throughput) instead of their 50 ms bins.
pub const REQUEST_MIN_WIDTH: f32 = 4.0;
/// Below this spacing between samples, chart points are not dotted.
pub const DOT_MIN_SPACING: f32 = 6.0;
/// Value axis labels left of a time-series plot.
pub const AXIS_WIDTH: f32 = 44.0;
/// Space right of a time-series plot for percentile labels.
pub const RIGHT_GUTTER: f32 = 84.0;
/// Time axis labels below a plot.
pub const AXIS_HEIGHT: f32 = 20.0;
/// Space above a time-series plot for the unit label.
pub const PLOT_TOP: f32 = 16.0;
pub const TICK_SPACING_X: f32 = 90.0;
pub const TICK_SPACING_Y: f32 = 36.0;
pub const TICK_SPACING_BOX: f32 = 56.0;
/// Label column left of box plots.
pub const BOX_LABEL: f32 = 96.0;
/// Height of one box plot row.
pub const BOX_ROW: f32 = 24.0;
/// Label column left of the RSSI sparkline.
pub const SPARK_AXIS: f32 = 32.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn lum(c: [u8; 3]) -> f64 {
        0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Window and content backgrounds per appearance: macOS, Windows
    /// (Mica-like), GNOME (Adwaita), plus pure white and near-black.
    const LIGHT_BG: [u32; 5] = [0xFFFFFF, 0xECECEC, 0xF3F3F3, 0xFAFAFA, 0xF5F5F5];
    const DARK_BG: [u32; 5] = [0x1E1E1E, 0x323232, 0x202020, 0x242424, 0x0B0B0C];

    #[test]
    fn data_colours_reach_graphics_contrast() {
        // WCAG 1.4.11: lines and markers need 3:1 against the background.
        // Values themselves are drawn in the label colour.
        for c in Color::ALL {
            let Some([light, dark]) = c.rgb() else {
                continue;
            };
            for bg in LIGHT_BG {
                let r = contrast(light, hex(bg));
                assert!(r >= 3.0, "{c:?} light {r:.2}:1 on {bg:06X}");
            }
            for bg in DARK_BG {
                let r = contrast(dark, hex(bg));
                assert!(r >= 3.0, "{c:?} dark {r:.2}:1 on {bg:06X}");
            }
        }
    }

    #[test]
    fn status_colours_reach_text_contrast() {
        // Errors and warnings are text in the status line.
        for c in [Color::Error, Color::Warning] {
            let [light, dark] = c.rgb().expect("data colour");
            for bg in LIGHT_BG {
                assert!(contrast(light, hex(bg)) >= 4.5, "{c:?} light on {bg:06X}");
            }
            for bg in DARK_BG {
                assert!(contrast(dark, hex(bg)) >= 4.5, "{c:?} dark on {bg:06X}");
            }
        }
    }

    #[test]
    fn plotted_series_are_distinct() {
        // Series drawn as lines or ticks; jitter is a value, not a line.
        let s = [Color::Download, Color::Upload, Color::Latency, Color::Loss];
        for mode in 0..2 {
            for (i, a) in s.iter().enumerate() {
                for b in &s[i + 1..] {
                    let (x, y) = (a.rgb().unwrap()[mode], b.rgb().unwrap()[mode]);
                    let d: i32 = (0..3)
                        .map(|k| (i32::from(x[k]) - i32::from(y[k])).pow(2))
                        .sum();
                    assert!(f64::from(d).sqrt() > 90.0, "{a:?} vs {b:?} ({mode})");
                }
            }
        }
    }
}
