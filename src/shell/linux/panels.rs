//! Everything outside the charts and the log: the header bar controls and
//! status, the headline values, chart legends, the breakdown columns and
//! the connection info sidebar. Each part rebuilds its widgets only when
//! the frame's structure changes and otherwise updates them in place.

use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;

use super::chart::Chart;
use super::style;
use super::tip::Tipped;
use crate::model::RunMode;
use crate::view::chart::{boxplot, rssi};
use crate::view::core::ChartId;
use crate::view::frame::{self, Column, Controls, Headline, Item, Kv, Legend, Section, Status};
use crate::view::palette::Color;

/// Spacing tokens from DESIGN.md.
pub const S4: i32 = 4;
pub const S8: i32 = 8;
pub const S12: i32 = 12;
pub const S16: i32 = 16;
pub const S24: i32 = 24;
pub const S32: i32 = 32;

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).build()
}

/// Sets `l`'s colour classes, keeping `base`.
fn colour(l: &gtk::Label, base: &[&str], c: Color) {
    let classes: Vec<&str> = base.iter().chain(style::fg(c)).copied().collect();
    l.set_css_classes(&classes);
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

// ---- Header bar ----

pub struct Header {
    pub bar: adw::HeaderBar,
    pub one_shot: gtk::ToggleButton,
    pub continuous: gtk::ToggleButton,
    pub start: gtk::Button,
    pub pause: gtk::Button,
    pub reset: gtk::Button,
    pub export: gtk::Button,
    pub settings: gtk::Button,
    pub sidebar: gtk::ToggleButton,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    elapsed_label: gtk::Label,
    elapsed: gtk::Label,
}

impl Header {
    pub fn new() -> Header {
        let one_shot = gtk::ToggleButton::with_label("One-shot");
        let continuous = gtk::ToggleButton::with_label("Continuous");
        continuous.set_group(Some(&one_shot));
        one_shot.set_active(true);
        let modes = gtk::Box::builder().css_classes(["linked"]).build();
        modes.append(&one_shot);
        modes.append(&continuous);

        let start = gtk::Button::with_label("Start");
        let pause = gtk::Button::with_label("Pause");
        let reset = gtk::Button::with_label("Reset");
        let export = gtk::Button::with_label("Export…");
        let settings = gtk::Button::with_label("Settings");
        let sidebar = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Connection info")
            .active(true)
            .build();

        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .has_tooltip(true)
            .build();
        // The full text when it is cut off.
        status.connect_query_tooltip(|l, _, _, _, tooltip| {
            let cut = l.layout().is_ellipsized();
            if cut {
                tooltip.set_text(Some(&l.text()));
            }
            cut
        });
        let elapsed_label = gtk::Label::builder()
            .label("Elapsed")
            .css_classes(["dim-label"])
            .visible(false)
            .build();
        let elapsed = gtk::Label::builder()
            .css_classes(["numeric"])
            .visible(false)
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, S8);
        line.append(&status);
        line.append(&elapsed_label);
        line.append(&elapsed);
        let progress = gtk::ProgressBar::builder()
            .css_classes(["rst-progress"])
            .build();
        let title = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(S4)
            .valign(gtk::Align::Center)
            .width_request(300)
            .build();
        title.append(&line);
        title.append(&progress);

        let bar = adw::HeaderBar::new();
        bar.set_title_widget(Some(&title));
        bar.pack_start(&modes);
        bar.pack_start(&start);
        bar.pack_start(&pause);
        bar.pack_start(&reset);
        bar.pack_end(&sidebar);
        bar.pack_end(&settings);
        bar.pack_end(&export);
        Header {
            bar,
            one_shot,
            continuous,
            start,
            pause,
            reset,
            export,
            settings,
            sidebar,
            status,
            progress,
            elapsed_label,
            elapsed,
        }
    }

    pub fn controls(&self, c: &Controls) {
        let (on, off) = match c.mode {
            RunMode::OneShot => (&self.one_shot, &self.continuous),
            RunMode::Continuous => (&self.continuous, &self.one_shot),
        };
        on.set_active(true);
        off.set_active(false);
        self.one_shot.set_sensitive(c.mode_enabled);
        self.continuous.set_sensitive(c.mode_enabled);
        self.start
            .set_label(if c.active { "Stop" } else { "Start" });
        if c.active {
            self.start.remove_css_class("suggested-action");
        } else {
            self.start.add_css_class("suggested-action");
        }
        self.pause
            .set_label(if c.paused { "Resume" } else { "Pause" });
        self.pause.set_sensitive(c.active);
        self.reset.set_sensitive(c.can_reset);
        self.export.set_sensitive(c.can_export);
    }

    pub fn status(&self, s: &Status) {
        self.status.set_text(&s.text);
        colour(&self.status, &[], s.color);
    }

    pub fn progress(&self, p: Option<f32>) {
        self.progress.set_fraction(f64::from(p.unwrap_or(0.0)));
    }

    pub fn elapsed(&self, e: Option<&str>) {
        self.elapsed.set_text(e.unwrap_or_default());
        self.elapsed.set_visible(e.is_some());
        self.elapsed_label.set_visible(e.is_some());
    }
}

// ---- Headline values ----

struct HeadlineItem {
    swatch: gtk::Box,
    label: gtk::Label,
    value: gtk::Label,
    unit: gtk::Label,
    tip: Rc<Tipped>,
}

pub struct Headlines {
    pub root: gtk::Box,
    items: Vec<HeadlineItem>,
}

impl Headlines {
    pub fn new() -> Headlines {
        Headlines {
            root: gtk::Box::new(gtk::Orientation::Horizontal, S32),
            items: Vec::new(),
        }
    }

    pub fn update(&mut self, hs: &[Headline]) {
        if self.items.len() != hs.len() {
            clear(&self.root);
            self.items = hs.iter().map(|_| self.item()).collect();
        }
        for (it, h) in self.items.iter().zip(hs) {
            it.swatch
                .set_css_classes(&["rst-swatch", &style::bg(h.color)]);
            it.label.set_text(h.label);
            let (value, unit) = match &h.value {
                Some(v) => (v.as_str(), h.unit),
                None => (frame::MISSING, ""),
            };
            let size = if h.hero { "rst-hero" } else { "rst-secondary" };
            it.value.set_css_classes(&["numeric", size]);
            it.value.set_text(value);
            let unit_size = if h.hero {
                "rst-hero-unit"
            } else {
                "rst-secondary-unit"
            };
            it.unit.set_css_classes(&["dim-label", unit_size]);
            it.unit.set_text(unit);
            it.unit.set_visible(!unit.is_empty());
            it.tip.set(&h.tip);
        }
    }

    fn item(&self) -> HeadlineItem {
        let swatch = gtk::Box::builder().valign(gtk::Align::Center).build();
        let label = label("");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, S8);
        top.append(&swatch);
        top.append(&label);
        let value = gtk::Label::builder()
            .valign(gtk::Align::BaselineFill)
            .build();
        let unit = gtk::Label::builder()
            .valign(gtk::Align::BaselineFill)
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, S4);
        line.append(&value);
        line.append(&unit);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&top);
        root.append(&line);
        let tip = Tipped::attach(&root);
        self.root.append(&root);
        HeadlineItem {
            swatch,
            label,
            value,
            unit,
            tip,
        }
    }
}

// ---- Legends ----

/// Legend entries above a chart: a line sample (or a tick for loss
/// events) in the series colour and its label.
pub fn legend(row: &gtk::Box, items: &[Legend]) {
    clear(row);
    for l in items {
        let sample = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes([
                if l.tick {
                    "rst-legend-tick"
                } else {
                    "rst-legend-line"
                },
                style::bg(l.color).as_str(),
            ])
            .build();
        let text = gtk::Label::builder()
            .label(l.label)
            .css_classes(["dim-label"])
            .build();
        let item = gtk::Box::new(gtk::Orientation::Horizontal, S4);
        item.append(&sample);
        item.append(&text);
        row.append(&item);
    }
}

// ---- Label/value rows ----

struct KvRow {
    value: gtk::Label,
    tip: Rc<Tipped>,
}

impl KvRow {
    /// Long labels and values (addresses, cipher names) wrap rather than
    /// being cut off.
    fn new(kv: &Kv) -> (gtk::Box, KvRow) {
        let name = gtk::Label::builder()
            .label(&kv.label)
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["dim-label"])
            .build();
        let value = gtk::Label::builder()
            .xalign(1.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .justify(gtk::Justification::Right)
            .css_classes(["numeric"])
            .build();
        let root = gtk::Box::new(gtk::Orientation::Horizontal, S12);
        root.append(&name);
        root.append(&value);
        let row = KvRow {
            value,
            tip: Tipped::attach(&root),
        };
        row.update(kv);
        (root, row)
    }

    fn update(&self, kv: &Kv) {
        match kv.value.as_deref() {
            Some(v) => {
                self.value.set_text(v);
                self.value.set_css_classes(&["numeric"]);
            }
            None => {
                self.value.set_text(frame::MISSING);
                self.value.set_css_classes(&["numeric", "dim-label"]);
            }
        }
        self.tip.set(&kv.tip);
    }
}

// ---- Breakdown columns ----

#[derive(Clone, PartialEq)]
enum Shape {
    Sub(&'static str),
    Boxes(boxplot::BoxId, usize),
    Kv(String),
}

fn shape(c: &Column) -> (&'static str, Vec<Shape>) {
    let items = c
        .items
        .iter()
        .map(|i| match i {
            Item::Sub(s) => Shape::Sub(s),
            Item::Boxes { id, rows } => Shape::Boxes(*id, *rows),
            Item::Kv(kv) => Shape::Kv(kv.label.clone()),
        })
        .collect();
    (c.title, items)
}

struct ColumnUi {
    shape: (&'static str, Vec<Shape>),
    rows: Vec<KvRow>,
    charts: Vec<Rc<Chart>>,
}

pub struct Columns {
    pub root: gtk::Box,
    cols: Vec<(gtk::Box, ColumnUi)>,
}

impl Columns {
    pub fn new() -> Columns {
        Columns {
            root: gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(S24)
                .homogeneous(true)
                .build(),
            cols: Vec::new(),
        }
    }

    /// Box plot charts, for scene and hover updates.
    pub fn charts(&self) -> impl Iterator<Item = &Rc<Chart>> {
        self.cols.iter().flat_map(|(_, c)| c.charts.iter())
    }

    /// Updates in place; rebuilds a column whose items changed.
    pub fn update(&mut self, cols: &[Column]) {
        while self.cols.len() > cols.len() {
            if let Some((b, _)) = self.cols.pop() {
                self.root.remove(&b);
            }
        }
        for (i, c) in cols.iter().enumerate() {
            let sh = shape(c);
            if let Some((_, ui)) = self.cols.get(i)
                && ui.shape == sh
            {
                let kvs = c.items.iter().filter_map(|it| match it {
                    Item::Kv(kv) => Some(kv),
                    _ => None,
                });
                for (row, kv) in ui.rows.iter().zip(kvs) {
                    row.update(kv);
                }
                continue;
            }
            let (b, ui) = column(c, sh);
            match self.cols.get_mut(i) {
                Some(slot) => {
                    self.root.insert_child_after(&b, Some(&slot.0));
                    self.root.remove(&slot.0);
                    *slot = (b, ui);
                }
                None => {
                    self.root.append(&b);
                    self.cols.push((b, ui));
                }
            }
        }
    }
}

fn column(c: &Column, shape: (&'static str, Vec<Shape>)) -> (gtk::Box, ColumnUi) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(S4)
        .build();
    let title = label(c.title);
    title.add_css_class("heading");
    title.set_margin_bottom(S4);
    root.append(&title);
    let mut ui = ColumnUi {
        shape,
        rows: Vec::new(),
        charts: Vec::new(),
    };
    for it in &c.items {
        match it {
            Item::Sub(s) => {
                let l = label(s);
                l.set_css_classes(&["caption", "dim-label"]);
                l.set_margin_top(S8);
                root.append(&l);
            }
            Item::Boxes { id, rows } => {
                let chart = Chart::new(ChartId::Box(*id), boxplot::height(*rows));
                root.append(&chart.root);
                ui.charts.push(chart);
            }
            Item::Kv(kv) => {
                let (b, row) = KvRow::new(kv);
                root.append(&b);
                ui.rows.push(row);
            }
        }
    }
    (root, ui)
}

// ---- Connection info ----

type InfoShape = (Vec<(&'static str, Vec<String>)>, bool);

pub struct Info {
    pub root: gtk::Box,
    shape: Option<InfoShape>,
    rows: Vec<KvRow>,
    rssi: Option<Rc<Chart>>,
}

impl Info {
    pub fn new() -> Info {
        Info {
            root: gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(S16)
                .margin_top(S16)
                .margin_bottom(S16)
                .margin_start(S16)
                .margin_end(S16)
                .build(),
            shape: None,
            rows: Vec::new(),
            rssi: None,
        }
    }

    pub fn rssi(&self) -> Option<&Rc<Chart>> {
        self.rssi.as_ref()
    }

    /// Updates in place; rebuilds when sections or rows change.
    pub fn update(&mut self, sections: &[Section], show_rssi: bool) {
        let sh: InfoShape = (
            sections
                .iter()
                .map(|s| (s.title, s.rows.iter().map(|r| r.label.clone()).collect()))
                .collect(),
            show_rssi,
        );
        if self.shape.as_ref() == Some(&sh) {
            for (row, kv) in self.rows.iter().zip(sections.iter().flat_map(|s| &s.rows)) {
                row.update(kv);
            }
            return;
        }
        clear(&self.root);
        self.rows.clear();
        self.rssi = None;
        for s in sections {
            let b = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(S4)
                .build();
            let title = label(s.title);
            title.add_css_class("heading");
            b.append(&title);
            for kv in &s.rows {
                let (r, row) = KvRow::new(kv);
                b.append(&r);
                self.rows.push(row);
            }
            if show_rssi && s.title == "Wi-Fi" {
                let chart = Chart::new(ChartId::Rssi, rssi::HEIGHT);
                chart.root.set_margin_top(S8);
                b.append(&chart.root);
                self.rssi = Some(chart);
            }
            self.root.append(&b);
        }
        self.shape = Some(sh);
    }
}
