//! Every design token from DESIGN.md. No color, size or spacing value is
//! written anywhere else in the UI.

use eframe::egui::{Color32, Margin, Vec2};

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

// Neutrals: near-black ramp without a blue tint.
pub const BG: Color32 = hex(0x0B0B0C);
pub const SURFACE: Color32 = hex(0x141416);
/// Hovered or pressed control surface, one step above `SURFACE`.
pub const SURFACE_RAISED: Color32 = hex(0x1C1C1F);
pub const BORDER: Color32 = hex(0x26262A);
pub const TEXT: Color32 = hex(0xF4F4F5);
pub const TEXT_2: Color32 = hex(0xA1A1AA);
/// Below 4.5:1, so it never carries information: disabled controls only.
pub const TEXT_3: Color32 = hex(0x71717A);

// Series colors, one per metric, identical in every chart, number and
// legend. Each reaches 7:1 against `BG` because headline values use them.
pub const DOWNLOAD: Color32 = hex(0xFF9500);
pub const UPLOAD: Color32 = hex(0x2EE0D6);
pub const LATENCY: Color32 = hex(0x7C9CFF);
pub const JITTER: Color32 = hex(0xFFE14D);
pub const LOSS: Color32 = hex(0xFF6B6B);
/// Gateway latency is drawn as a neutral reference line.
pub const GATEWAY: Color32 = TEXT_2;

// Status colors, only for real state.
pub const ERROR: Color32 = LOSS;
pub const WARNING: Color32 = hex(0xFFB547);

/// The one accent for interactive controls.
pub const ACCENT: Color32 = hex(0xE4E4E7);
pub const ON_ACCENT: Color32 = BG;

/// Opacity of the flat fill under a data line and of load-period bands.
pub const FILL_ALPHA: f32 = 0.12;
/// Fill of the throughput area in the overview strip.
pub const OVERVIEW_FILL_ALPHA: f32 = 0.35;
/// Min–max bars of aggregated buckets.
pub const ENVELOPE_ALPHA: f32 = 0.5;
/// Dimming of the overview strip outside the visible window.
pub const OUTSIDE_VIEW_ALPHA: f32 = 0.6;
pub const BAND_ALPHA: f32 = 0.07;
pub const PERCENTILE_ALPHA: f32 = 0.7;

pub fn tint(c: Color32, alpha: f32) -> Color32 {
    c.gamma_multiply(alpha)
}

// Type scale.
pub const HERO: f32 = 56.0;
pub const SECONDARY: f32 = 32.0;
pub const SECTION: f32 = 15.0;
pub const BODY: f32 = 13.0;
pub const SMALL: f32 = 11.0;
/// Unit text after a hero value.
pub const HERO_UNIT: f32 = SECTION;
/// Unit text after a secondary value.
pub const SECONDARY_UNIT: f32 = BODY;

// Weights, as variable-font `wght` values.
pub const WEIGHT_REGULAR: f32 = 400.0;
pub const WEIGHT_SEMIBOLD: f32 = 600.0;
/// Optical size used for text at body sizes (SF's Text cut).
pub const OPSZ_TEXT: f32 = 17.0;
/// Optical size used for the large values (SF's Display cut).
pub const OPSZ_DISPLAY: f32 = 28.0;

// Spacing: 4 px grid.
pub const S4: f32 = 4.0;
pub const S8: f32 = 8.0;
pub const S12: f32 = 12.0;
pub const S16: f32 = 16.0;
pub const S24: f32 = 24.0;
pub const S32: f32 = 32.0;

pub const RADIUS: u8 = 3;
pub const STROKE: f32 = 1.0;
pub const DATA_LINE: f32 = 1.5;
pub const POINT_RADIUS: f32 = 2.0;
pub const POINT_HOVER_RADIUS: f32 = 3.5;
pub const DASH: f32 = 4.0;
pub const DASH_GAP: f32 = 3.0;
/// Pointer distance that counts as hovering a point.
pub const HOVER_PX: f32 = 6.0;
/// Gap between the hovered point and its tooltip.
pub const TOOLTIP_OFFSET: f32 = S12;
pub const LOSS_TICK: f32 = S8;

// Window and layout.
pub const WINDOW_DEFAULT: Vec2 = Vec2::new(1440.0, 920.0);
pub const WINDOW_MIN: Vec2 = Vec2::new(1180.0, 760.0);
pub const SIDE_WIDTH: f32 = 300.0;
pub const PANEL_MARGIN: Margin = Margin::same(S16 as i8);
pub const THROUGHPUT_HEIGHT: f32 = 196.0;
pub const LATENCY_HEIGHT: f32 = 148.0;
pub const OVERVIEW_HEIGHT: f32 = S32;
pub const AXIS_WIDTH: f32 = 44.0;
/// Space right of a time-series plot for percentile labels.
pub const RIGHT_GUTTER: f32 = 72.0;
pub const AXIS_HEIGHT: f32 = 20.0;
pub const BOX_ROW: f32 = 24.0;
pub const TABLE_ROW: f32 = 18.0;
pub const BUTTON_HEIGHT: f32 = 28.0;
pub const BUTTON_PAD: f32 = S12;
pub const PROGRESS_HEIGHT: f32 = 2.0;
pub const LOG_HEIGHT: f32 = 220.0;
/// Minimum horizontal distance between x-axis tick labels.
pub const TICK_SPACING_X: f32 = 90.0;
pub const TICK_SPACING_Y: f32 = 36.0;
pub const TICK_SPACING_BOX: f32 = 56.0;
/// Label column left of box plots.
pub const BOX_LABEL: f32 = 96.0;
/// Label column of the settings rows.
pub const SETTINGS_LABEL: f32 = 132.0;
pub const TIP_MIN_W: f32 = 160.0;
pub const TIP_MAX_W: f32 = 320.0;
/// Length of a legend line sample.
pub const LEGEND_SWATCH: f32 = S12;
/// Farthest a crosshair snaps to a sample.
pub const SNAP_PX: f32 = 40.0;
/// Below this spacing between samples, chart points are not dotted.
pub const DOT_MIN_SPACING: f32 = 6.0;
/// Points per wheel "line" when a mouse reports lines.
pub const WHEEL_LINE: f32 = 24.0;
// Log table columns.
pub const COL_TIME: f32 = 92.0;
pub const COL_S: f32 = 56.0;
pub const COL_M: f32 = 64.0;
pub const COL_L: f32 = 72.0;
pub const COL_XL: f32 = 80.0;
pub const COL_XXL: f32 = 96.0;
pub const COL_USE: f32 = 104.0;
pub const COL_ADDR: f32 = 150.0;
pub const COL_CIPHER: f32 = 220.0;

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

    fn lum(c: Color32) -> f64 {
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn values_and_labels_reach_aaa() {
        for (name, c) in [
            ("text", TEXT),
            ("download", DOWNLOAD),
            ("upload", UPLOAD),
            ("latency", LATENCY),
            ("jitter", JITTER),
            ("loss", LOSS),
        ] {
            let r = contrast(c, BG);
            assert!(r >= 7.0, "{name} {r:.2}:1 against bg");
            assert!(contrast(c, SURFACE) >= 6.5, "{name} on surface");
        }
    }

    #[test]
    fn secondary_text_reaches_aa() {
        assert!(contrast(TEXT_2, BG) >= 4.5);
        assert!(contrast(TEXT_2, SURFACE) >= 4.5);
        assert!(contrast(WARNING, BG) >= 4.5);
        assert!(contrast(ON_ACCENT, ACCENT) >= 7.0);
    }

    #[test]
    fn series_are_distinct() {
        // Minimum pairwise distance in RGB; enough to tell lines apart.
        let s = [DOWNLOAD, UPLOAD, LATENCY, JITTER, LOSS];
        for (i, a) in s.iter().enumerate() {
            for b in &s[i + 1..] {
                let d = (i32::from(a.r()) - i32::from(b.r())).pow(2)
                    + (i32::from(a.g()) - i32::from(b.g())).pow(2)
                    + (i32::from(a.b()) - i32::from(b.b())).pow(2);
                assert!(f64::from(d).sqrt() > 90.0, "{a:?} vs {b:?}");
            }
        }
    }
}
