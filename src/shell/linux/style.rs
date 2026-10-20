//! Colour tokens for GTK: CSS classes for widgets and RGBA values for
//! Cairo. Data colours are the palette's light/dark pairs, swapped when
//! the system appearance changes; neutrals are libadwaita's named colours
//! (label colour, its dimmed variants, the window background, the accent).

use std::fmt::Write;

use gtk::gdk;

use crate::view::palette::Color;

/// Opacity of `.dim-label` in libadwaita: secondary text.
const TEXT2_ALPHA: f32 = 0.55;
/// Tertiary text and the crosshair.
const TEXT3_ALPHA: f32 = 0.35;
/// libadwaita draws borders and separators at this opacity of the label
/// colour.
const GRID_ALPHA: f32 = 0.15;

fn name(c: Color) -> &'static str {
    match c {
        Color::Download => "download",
        Color::Upload => "upload",
        Color::Latency => "latency",
        Color::Jitter => "jitter",
        Color::Loss => "loss",
        Color::Text => "text",
        Color::Text2 => "text2",
        Color::Text3 => "text3",
        Color::Grid => "grid",
        Color::Background => "background",
        Color::Accent => "accent",
        Color::Warning => "warning",
        Color::Error => "error",
    }
}

/// CSS classes that give a label the colour `c`.
pub fn fg(c: Color) -> &'static [&'static str] {
    match c {
        // The label colour is inherited.
        Color::Text => &[],
        Color::Text2 => &["dim-label"],
        Color::Download => &["rst-fg-download"],
        Color::Upload => &["rst-fg-upload"],
        Color::Latency => &["rst-fg-latency"],
        Color::Jitter => &["rst-fg-jitter"],
        Color::Loss => &["rst-fg-loss"],
        Color::Text3 => &["rst-fg-text3"],
        Color::Grid => &["rst-fg-grid"],
        Color::Background => &["rst-fg-background"],
        Color::Accent => &["rst-fg-accent"],
        Color::Warning => &["rst-fg-warning"],
        Color::Error => &["rst-fg-error"],
    }
}

/// CSS class that fills a box (a swatch, a legend sample) with `c`.
pub fn bg(c: Color) -> String {
    format!("rst-bg-{}", name(c))
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The app's stylesheet for the light or dark appearance.
pub fn css(dark: bool) -> String {
    let mut s = String::from(
        "\
.rst-hero { font-size: 56px; }
.rst-secondary { font-size: 32px; }
.rst-hero-unit { font-size: 15px; }
.rst-secondary-unit { font-size: 13px; }
.rst-swatch { min-width: 10px; min-height: 10px; border-radius: 2px; }
.rst-legend-line { min-width: 16px; min-height: 2px; }
.rst-legend-tick { min-width: 2px; min-height: 10px; }
.rst-tip { font-size: 11px; }
.rst-tip-box {
  background-color: @popover_bg_color;
  color: @popover_fg_color;
  border: 1px solid alpha(currentColor, 0.15);
  border-radius: 6px;
  padding: 6px 8px;
}
.rst-progress trough, .rst-progress progress { min-height: 2px; }
.rst-fg-background { color: @window_bg_color; }
.rst-fg-accent { color: @accent_color; }
.rst-bg-text { background-color: currentColor; }
.rst-bg-background { background-color: @window_bg_color; }
.rst-bg-accent { background-color: @accent_bg_color; }
",
    );
    for (n, a) in [
        ("text2", TEXT2_ALPHA),
        ("text3", TEXT3_ALPHA),
        ("grid", GRID_ALPHA),
    ] {
        let _ = writeln!(s, ".rst-fg-{n} {{ color: alpha(currentColor, {a}); }}");
        let _ = writeln!(
            s,
            ".rst-bg-{n} {{ background-color: alpha(currentColor, {a}); }}"
        );
    }
    for c in Color::ALL {
        if let Some(rgb) = c.rgb() {
            let v = hex(rgb[usize::from(dark)]);
            let n = name(c);
            let _ = writeln!(s, ".rst-fg-{n} {{ color: {v}; }}");
            let _ = writeln!(s, ".rst-bg-{n} {{ background-color: {v}; }}");
        }
    }
    s
}

/// Every token resolved to a Cairo colour for one drawing.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The label colour where the chart is drawn.
    pub fg: gdk::RGBA,
    pub background: gdk::RGBA,
    pub accent: gdk::RGBA,
    pub dark: bool,
}

fn with_alpha(c: gdk::RGBA, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), c.alpha() * a)
}

impl Palette {
    pub fn rgba(&self, c: Color) -> gdk::RGBA {
        if let Some(rgb) = c.rgb() {
            let [r, g, b] = rgb[usize::from(self.dark)];
            let f = |v: u8| f32::from(v) / 255.0;
            return gdk::RGBA::new(f(r), f(g), f(b), 1.0);
        }
        match c {
            Color::Text2 => with_alpha(self.fg, TEXT2_ALPHA),
            Color::Text3 => with_alpha(self.fg, TEXT3_ALPHA),
            Color::Grid => with_alpha(self.fg, GRID_ALPHA),
            Color::Background => self.background,
            Color::Accent => self.accent,
            _ => self.fg,
        }
    }
}
