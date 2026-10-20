//! JSON for the values the SwiftUI shell decodes: the frame, hovers, log
//! pages and the palette. Hand-written like the export; field names match
//! the `Codable` types in `swift/Model.swift`.

use crate::net::json::escape;
use crate::view::chart::Hover;
use crate::view::frame::{Column, Frame, Item, Kv, Legend, Section};
use crate::view::log::{self, Row, Tab};
use crate::view::palette::{self, Color};
use crate::view::scene::Point;
use crate::view::tip::{Tip, TipRow};

pub trait ToJson {
    fn json(&self, out: &mut String);
}

macro_rules! obj {
    ($out:expr, $($k:literal => $v:expr),* $(,)?) => {{
        let out: &mut String = $out;
        out.push('{');
        let mut _first = true;
        $(
            if !_first {
                out.push(',');
            }
            _first = false;
            escape($k, out);
            out.push(':');
            ToJson::json(&$v, out);
        )*
        out.push('}');
    }};
}

impl ToJson for str {
    fn json(&self, out: &mut String) {
        escape(self, out);
    }
}

impl ToJson for String {
    fn json(&self, out: &mut String) {
        escape(self, out);
    }
}

impl<T: ToJson + ?Sized> ToJson for &T {
    fn json(&self, out: &mut String) {
        (**self).json(out);
    }
}

impl ToJson for bool {
    fn json(&self, out: &mut String) {
        out.push_str(if *self { "true" } else { "false" });
    }
}

macro_rules! ints {
    ($($t:ty),*) => {$(
        impl ToJson for $t {
            fn json(&self, out: &mut String) {
                out.push_str(&self.to_string());
            }
        }
    )*};
}
ints!(u8, u32, u64, usize);

impl ToJson for f32 {
    fn json(&self, out: &mut String) {
        if self.is_finite() {
            out.push_str(&self.to_string());
        } else {
            out.push_str("null");
        }
    }
}

impl<T: ToJson> ToJson for Option<T> {
    fn json(&self, out: &mut String) {
        match self {
            Some(v) => v.json(out),
            None => out.push_str("null"),
        }
    }
}

impl<T: ToJson> ToJson for [T] {
    fn json(&self, out: &mut String) {
        out.push('[');
        for (i, v) in self.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            v.json(out);
        }
        out.push(']');
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn json(&self, out: &mut String) {
        self.as_slice().json(out);
    }
}

impl<T: ToJson, const N: usize> ToJson for [T; N] {
    fn json(&self, out: &mut String) {
        self.as_slice().json(out);
    }
}

impl ToJson for Color {
    fn json(&self, out: &mut String) {
        (*self as u8).json(out);
    }
}

impl ToJson for TipRow {
    fn json(&self, out: &mut String) {
        match self {
            TipRow::Title(s) => obj!(out, "title" => s),
            TipRow::Pair(k, v) => obj!(out, "key" => k, "value" => v),
            TipRow::Note(s) => obj!(out, "note" => s),
        }
    }
}

impl ToJson for Tip {
    fn json(&self, out: &mut String) {
        self.rows.json(out);
    }
}

impl ToJson for Kv {
    fn json(&self, out: &mut String) {
        obj!(out, "label" => self.label, "value" => self.value, "tip" => self.tip);
    }
}

impl ToJson for Item {
    fn json(&self, out: &mut String) {
        match self {
            Item::Sub(s) => obj!(out, "sub" => s),
            Item::Boxes { id, rows } => obj!(out, "boxes" => (*id as u8), "rows" => rows),
            Item::Kv(kv) => obj!(out, "kv" => kv),
        }
    }
}

impl ToJson for Column {
    fn json(&self, out: &mut String) {
        obj!(out, "title" => self.title, "items" => self.items);
    }
}

impl ToJson for Section {
    fn json(&self, out: &mut String) {
        obj!(out, "title" => self.title, "rows" => self.rows);
    }
}

impl ToJson for Legend {
    fn json(&self, out: &mut String) {
        obj!(out, "label" => self.label, "color" => self.color, "tick" => self.tick);
    }
}

impl ToJson for Point {
    fn json(&self, out: &mut String) {
        obj!(out, "x" => self.x, "y" => self.y);
    }
}

/// `next_tick_ms`: when to ask for a frame again without new data.
pub fn frame(f: &Frame, next_tick_ms: Option<u32>, out: &mut String) {
    out.clear();
    let headline: Vec<String> = f
        .headline
        .iter()
        .map(|h| {
            let mut s = String::new();
            obj!(&mut s,
                "label" => h.label,
                "value" => h.value,
                "unit" => h.unit,
                "color" => h.color,
                "hero" => h.hero,
                "tip" => h.tip,
            );
            s
        })
        .collect();
    let settings: Vec<String> = f
        .settings
        .rows
        .iter()
        .map(|r| {
            let mut s = String::new();
            obj!(&mut s,
                "id" => (r.id as u8),
                "label" => r.label,
                "options" => r.options,
                "selected" => r.selected,
            );
            s
        })
        .collect();
    let c = &f.controls;
    let t = &f.tray;
    let tray_rows: Vec<[&str; 2]> = t
        .rows
        .iter()
        .map(|(k, v)| [k.as_str(), v.as_str()])
        .collect();
    let tabs = [
        Tab::Requests.label(f.log.requests),
        Tab::Connections.label(f.log.connections),
        Tab::Errors.label(f.log.errors),
    ];
    obj!(out,
        "headline" => Raw(&headline),
        "status" => Status(&f.status.text, f.status.color),
        "progress" => f.progress,
        "elapsed" => f.elapsed,
        "controls" => Controls(c),
        "settings" => SettingsJ(&settings, &f.settings.data_per_hour),
        "columns" => f.columns,
        "info" => f.info,
        "rssi" => f.rssi,
        "log_tabs" => tabs,
        "log_counts" => [f.log.requests, f.log.connections, f.log.errors],
        "tray" => TrayJ(t, &tray_rows),
        "legends" => LegendsJ(&f.legends.throughput, &f.legends.latency),
        "export_name" => f.export_name,
        "next_tick_ms" => next_tick_ms,
    );
}

/// Pre-encoded JSON values, emitted as an array.
struct Raw<'a>(&'a [String]);
impl ToJson for Raw<'_> {
    fn json(&self, out: &mut String) {
        out.push('[');
        for (i, v) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(v);
        }
        out.push(']');
    }
}

struct Status<'a>(&'a str, Color);
impl ToJson for Status<'_> {
    fn json(&self, out: &mut String) {
        obj!(out, "text" => self.0, "color" => self.1);
    }
}

struct Controls<'a>(&'a crate::view::frame::Controls);
impl ToJson for Controls<'_> {
    fn json(&self, out: &mut String) {
        let c = self.0;
        let mode: u8 = match c.mode {
            crate::model::RunMode::OneShot => 0,
            crate::model::RunMode::Continuous => 1,
        };
        obj!(out,
            "mode" => mode,
            "mode_enabled" => c.mode_enabled,
            "active" => c.active,
            "paused" => c.paused,
            "can_reset" => c.can_reset,
            "can_export" => c.can_export,
        );
    }
}

struct SettingsJ<'a>(&'a [String], &'a Kv);
impl ToJson for SettingsJ<'_> {
    fn json(&self, out: &mut String) {
        obj!(out, "rows" => Raw(self.0), "data_per_hour" => self.1);
    }
}

struct TrayJ<'a>(&'a crate::view::frame::Tray, &'a [[&'a str; 2]]);
impl ToJson for TrayJ<'_> {
    fn json(&self, out: &mut String) {
        let t = self.0;
        obj!(out,
            "title" => t.title,
            "rows" => self.1,
            "status" => t.status,
            "one_shot_running" => t.one_shot_running,
            "continuous_running" => t.continuous_running,
            "paused" => t.paused,
        );
    }
}

struct LegendsJ<'a>(&'a [Legend], &'a [Legend]);
impl ToJson for LegendsJ<'_> {
    fn json(&self, out: &mut String) {
        obj!(out, "throughput" => self.0, "latency" => self.1);
    }
}

struct Highlight(Point, Color);
impl ToJson for Highlight {
    fn json(&self, out: &mut String) {
        obj!(out, "x" => self.0.x, "y" => self.0.y, "color" => self.1);
    }
}

pub fn hover(h: &Hover, out: &mut String) {
    out.clear();
    obj!(out,
        "crosshair" => h.crosshair,
        "highlight" => h.highlight.map(|(p, c)| Highlight(p, c)),
        "tip" => h.tip,
        "anchor" => h.anchor,
    );
}

impl ToJson for log::Cell {
    fn json(&self, out: &mut String) {
        obj!(out, "text" => self.text, "color" => self.color);
    }
}

impl ToJson for Row {
    fn json(&self, out: &mut String) {
        obj!(out, "id" => self.id, "cells" => self.cells, "tip" => self.tip);
    }
}

pub fn log_page(total: usize, rows: &[Row], out: &mut String) {
    out.clear();
    obj!(out, "total" => total, "rows" => rows);
}

impl ToJson for log::Column {
    fn json(&self, out: &mut String) {
        obj!(out, "title" => self.title, "numeric" => self.numeric, "width" => self.width);
    }
}

struct ColorJ(Color);
impl ToJson for ColorJ {
    fn json(&self, out: &mut String) {
        let rgb = self.0.rgb();
        obj!(out, "light" => rgb.map(|c| c[0]), "dark" => rgb.map(|c| c[1]));
    }
}

/// What never changes: data colours, log columns, the dash pattern.
pub fn fixed(out: &mut String) {
    out.clear();
    let colors: Vec<ColorJ> = Color::ALL.iter().map(|c| ColorJ(*c)).collect();
    let columns = [
        log::columns(Tab::Requests).to_vec(),
        log::columns(Tab::Connections).to_vec(),
        log::columns(Tab::Errors).to_vec(),
    ];
    obj!(out,
        "colors" => colors,
        "columns" => columns,
        "dash" => palette::DASH,
        "point_hover_radius" => palette::POINT_HOVER_RADIUS,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::derived::Cache;
    use crate::model::{Config, RunMode};
    use crate::net::json::Json;

    #[test]
    fn frame_json_parses_and_escapes_control_characters() {
        let mut s = crate::app::session::synthetic(1800.0);
        s.errors.push((1.0, "bad\u{1}name \"quoted\"\n".into()));
        let mut c = Cache::default();
        c.update(&s);
        let f = crate::view::frame::build(
            &s,
            &c.derived,
            RunMode::Continuous,
            &Config::default(),
            None,
        );
        let mut out = String::new();
        frame(&f, Some(500), &mut out);
        let j = Json::parse(&out).expect("frame JSON parses");
        assert_eq!(
            j.get("export_name").and_then(Json::as_str),
            Some(f.export_name.as_str())
        );
        assert_eq!(j.get("next_tick_ms").and_then(Json::as_f64), Some(500.0));
        let mut page = String::new();
        let rows = log::rows(&s, Tab::Errors, 0, 5);
        log_page(s.errors.len(), &rows, &mut page);
        assert!(Json::parse(&page).is_some(), "log page parses: {page}");
        let mut fx = String::new();
        fixed(&mut fx);
        assert!(Json::parse(&fx).is_some());
    }
}
