//! Tooltip content from the view layer's `Tip` rows, used both in GTK
//! tooltips (headline values, breakdown and connection rows, log rows) and
//! in the chart tip, which shows immediately and never covers the hovered
//! point.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, pango};

use crate::view::tip::{Tip, TipRow};

/// Wrap width of explanatory notes, in characters.
const NOTE_CHARS: i32 = 44;
/// Gap between the hovered point and the chart tip.
const GAP: i32 = 12;
/// Distance the chart tip keeps from the window edges.
const MARGIN: i32 = 4;

/// Built tip content. Moving along a chart changes the values but rarely
/// the kinds of rows, so the labels are kept and only their texts change.
struct Built {
    grid: gtk::Grid,
    shape: Vec<Kind>,
    /// Per row: the title, the pair's label and value, or the note.
    labels: Vec<gtk::Label>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Title,
    Pair,
    Note,
}

fn shape(tip: &Tip) -> Vec<Kind> {
    tip.rows
        .iter()
        .map(|r| match r {
            TipRow::Title(_) => Kind::Title,
            TipRow::Pair(..) => Kind::Pair,
            TipRow::Note(_) => Kind::Note,
        })
        .collect()
}

impl Built {
    /// The rows of `tip` as a grid: a title, label/value pairs with the
    /// values right-aligned in tabular figures, and wrapped notes.
    fn new(tip: &Tip) -> Built {
        let grid = gtk::Grid::builder()
            .row_spacing(2)
            .column_spacing(12)
            .css_classes(["rst-tip"])
            .build();
        let mut labels = Vec::new();
        for (row, r) in tip.rows.iter().enumerate() {
            let row = row as i32;
            match r {
                TipRow::Title(_) => {
                    let l = gtk::Label::builder().xalign(0.0).build();
                    let attrs = pango::AttrList::new();
                    attrs.insert(pango::AttrInt::new_weight(pango::Weight::Semibold));
                    l.set_attributes(Some(&attrs));
                    grid.attach(&l, 0, row, 2, 1);
                    labels.push(l);
                }
                TipRow::Pair(..) => {
                    let k = gtk::Label::builder()
                        .xalign(0.0)
                        .css_classes(["dim-label"])
                        .build();
                    let v = gtk::Label::builder()
                        .xalign(1.0)
                        .hexpand(true)
                        .css_classes(["numeric"])
                        .build();
                    grid.attach(&k, 0, row, 1, 1);
                    grid.attach(&v, 1, row, 1, 1);
                    labels.push(k);
                    labels.push(v);
                }
                TipRow::Note(_) => {
                    let l = gtk::Label::builder()
                        .xalign(0.0)
                        .wrap(true)
                        .wrap_mode(pango::WrapMode::WordChar)
                        .max_width_chars(NOTE_CHARS)
                        .css_classes(["dim-label"])
                        .build();
                    if row > 0 {
                        l.set_margin_top(4);
                    }
                    grid.attach(&l, 0, row, 2, 1);
                    labels.push(l);
                }
            }
        }
        let b = Built {
            grid,
            shape: shape(tip),
            labels,
        };
        b.set(tip);
        b
    }

    /// New texts for a tip of the same shape.
    fn set(&self, tip: &Tip) {
        let mut labels = self.labels.iter();
        let mut put = |text: &str| {
            if let Some(l) = labels.next()
                && l.text() != text
            {
                l.set_text(text);
            }
        };
        for r in &tip.rows {
            match r {
                TipRow::Title(t) | TipRow::Note(t) => put(t),
                TipRow::Pair(k, v) => {
                    put(k);
                    put(v);
                }
            }
        }
    }
}

/// `tip` as tooltip content.
pub fn content(tip: &Tip) -> gtk::Widget {
    Built::new(tip).grid.upcast()
}

/// A GTK tooltip whose rows can change; the content is built when the
/// tooltip is first shown after a change.
#[derive(Default)]
pub struct Tipped {
    tip: RefCell<Tip>,
    built: RefCell<Option<gtk::Widget>>,
}

impl Tipped {
    pub fn attach(widget: &impl IsA<gtk::Widget>) -> Rc<Tipped> {
        let t = Rc::new(Tipped::default());
        widget.set_has_tooltip(true);
        let weak = Rc::downgrade(&t);
        widget.connect_query_tooltip(move |_, _, _, _, tooltip| {
            let Some(t) = weak.upgrade() else {
                return false;
            };
            if t.tip.borrow().is_empty() {
                return false;
            }
            let w = t
                .built
                .borrow_mut()
                .get_or_insert_with(|| content(&t.tip.borrow()))
                .clone();
            tooltip.set_custom(Some(&w));
            true
        });
        t
    }

    pub fn set(&self, tip: &Tip) {
        if *self.tip.borrow() != *tip {
            *self.tip.borrow_mut() = tip.clone();
            self.built.borrow_mut().take();
        }
    }
}

/// The chart tip: a box in an overlay above the whole window content, so
/// it appears immediately, can extend past the chart, and never takes
/// pointer events from the chart under it.
pub struct Layer {
    pub overlay: gtk::Overlay,
    frame: gtk::Box,
    tip: RefCell<Option<Tip>>,
    built: RefCell<Option<Built>>,
    /// The hovered point in overlay coordinates.
    anchor: Cell<(i32, i32)>,
}

impl Layer {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Rc<Layer> {
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(child));
        let frame = gtk::Box::builder()
            .css_classes(["rst-tip-box"])
            .can_target(false)
            .visible(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .build();
        overlay.add_overlay(&frame);
        let layer = Rc::new(Layer {
            overlay,
            frame,
            tip: RefCell::new(None),
            built: RefCell::new(None),
            anchor: Cell::new((0, 0)),
        });
        let weak = Rc::downgrade(&layer);
        layer
            .overlay
            .connect_get_child_position(move |overlay, child| {
                let layer = weak.upgrade()?;
                (child == layer.frame.upcast_ref::<gtk::Widget>()).then(|| layer.place(overlay))
            });
        layer
    }

    /// Where the tip goes: beside the anchor (right, else left), centred on
    /// it vertically; above or below it when neither side has room.
    fn place(&self, overlay: &gtk::Overlay) -> gdk::Rectangle {
        let (_, w, _, _) = self.frame.measure(gtk::Orientation::Horizontal, -1);
        let (_, h, _, _) = self.frame.measure(gtk::Orientation::Vertical, w);
        let (ow, oh) = (overlay.width(), overlay.height());
        let (ax, ay) = self.anchor.get();
        let clamp = |v: i32, size: i32, room: i32| v.min(room - size - MARGIN).max(MARGIN);
        let (x, y) = if ax + GAP + w <= ow - MARGIN {
            (ax + GAP, clamp(ay - h / 2, h, oh))
        } else if ax - GAP - w >= MARGIN {
            (ax - GAP - w, clamp(ay - h / 2, h, oh))
        } else if ay - GAP - h >= MARGIN {
            (clamp(ax - w / 2, w, ow), ay - GAP - h)
        } else {
            (clamp(ax - w / 2, w, ow), ay + GAP)
        };
        gdk::Rectangle::new(x, y, w, h)
    }

    /// Show `tip` next to `anchor` (a point in `from`'s coordinates).
    pub fn show(&self, tip: &Tip, from: &impl IsA<gtk::Widget>, anchor: (f32, f32)) {
        let p = gtk::graphene::Point::new(anchor.0, anchor.1);
        let Some(p) = from.compute_point(&self.overlay, &p) else {
            self.hide();
            return;
        };
        if self.tip.borrow().as_ref() != Some(tip) {
            let mut built = self.built.borrow_mut();
            match built.as_ref() {
                Some(b) if b.shape == shape(tip) => b.set(tip),
                _ => {
                    if let Some(old) = built.take() {
                        self.frame.remove(&old.grid);
                    }
                    let b = Built::new(tip);
                    self.frame.append(&b.grid);
                    *built = Some(b);
                }
            }
            *self.tip.borrow_mut() = Some(tip.clone());
        }
        self.anchor
            .set((p.x().round() as i32, p.y().round() as i32));
        self.frame.set_visible(true);
        self.overlay.queue_allocate();
    }

    pub fn hide(&self) {
        self.frame.set_visible(false);
    }
}
