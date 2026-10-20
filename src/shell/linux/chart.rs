//! A chart: a drawing area showing a scene, a second drawing area above it
//! for the crosshair and highlighted point (so hovering never redraws the
//! data), and the pointer, wheel and gesture handling.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use super::{app, draw};
use crate::view::chart::Hover;
use crate::view::core::ChartId;
use crate::view::scene::Scene;

/// Zoom per wheel step (Ctrl+wheel).
const WHEEL_ZOOM: f64 = 1.25;
/// Points panned per horizontal wheel step.
const WHEEL_PAN: f64 = 48.0;

pub struct Chart {
    pub id: ChartId,
    pub root: gtk::Overlay,
    area: gtk::DrawingArea,
    layer: gtk::DrawingArea,
    scene: RefCell<Scene>,
    hover: RefCell<Hover>,
    /// Pointer position while it is over the chart.
    pointer: Cell<Option<(f32, f32)>>,
    /// Drag offset already applied as a pan.
    dragged: Cell<f64>,
    /// Pinch scale already applied as a zoom.
    pinched: Cell<f64>,
}

impl Chart {
    pub fn new(id: ChartId, height: f32) -> Rc<Chart> {
        let area = gtk::DrawingArea::builder()
            .content_height(height.round() as i32)
            .hexpand(true)
            .build();
        let layer = gtk::DrawingArea::builder().can_target(false).build();
        let root = gtk::Overlay::builder().child(&area).hexpand(true).build();
        root.add_overlay(&layer);
        let chart = Rc::new(Chart {
            id,
            root,
            area,
            layer,
            scene: RefCell::default(),
            hover: RefCell::default(),
            pointer: Cell::new(None),
            dragged: Cell::new(0.0),
            pinched: Cell::new(1.0),
        });
        chart.connect();
        chart
    }

    /// Size in points, once allocated.
    pub fn size(&self) -> Option<(f32, f32)> {
        let (w, h) = (self.area.width(), self.area.height());
        (w > 0 && h > 0).then_some((w as f32, h as f32))
    }

    pub fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer.get()
    }

    /// Takes a newly built scene; redraws only if it differs.
    pub fn set_scene(&self, sc: &Scene) {
        if *self.scene.borrow() != *sc {
            self.scene.borrow_mut().clone_from(sc);
            self.area.queue_draw();
            self.layer.queue_draw();
        }
    }

    /// Takes a hover result; true if it changed.
    pub fn set_hover(&self, h: Hover) -> bool {
        if *self.hover.borrow() == h {
            return false;
        }
        *self.hover.borrow_mut() = h;
        self.layer.queue_draw();
        true
    }

    /// The drawing area, which chart-local coordinates are relative to.
    pub fn area(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn redraw(&self) {
        self.area.queue_draw();
        self.layer.queue_draw();
    }

    fn time_series(&self) -> bool {
        matches!(self.id, ChartId::Throughput | ChartId::Latency)
    }

    fn connect(self: &Rc<Chart>) {
        let weak = Rc::downgrade(self);
        self.area.set_draw_func(move |area, cr, _, _| {
            if let Some(c) = weak.upgrade() {
                draw::scene(cr, area, &c.scene.borrow(), &app().palette(area));
            }
        });
        let weak = Rc::downgrade(self);
        self.layer.set_draw_func(move |layer, cr, _, _| {
            if let Some(c) = weak.upgrade() {
                let scale = f64::from(layer.scale_factor().max(1));
                let pal = app().palette(&c.area);
                draw::hover(cr, scale, &c.scene.borrow(), &c.hover.borrow(), &pal);
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_resize(move |_, _, _| {
            if let Some(c) = weak.upgrade() {
                app().refresh_chart(&c);
            }
        });

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            if let Some(c) = weak.upgrade() {
                c.pointer.set(Some((x as f32, y as f32)));
                app().hover(&c);
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(c) = weak.upgrade() {
                c.pointer.set(None);
                app().hover(&c);
            }
        });
        self.root.add_controller(motion);

        match self.id {
            ChartId::Throughput | ChartId::Latency | ChartId::Overview => {}
            _ => return,
        }

        // Double-click resets the time window.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, n, x, _| {
            let Some(c) = weak.upgrade() else { return };
            if n == 2 {
                app().view(|core| core.reset_view());
            } else if c.id == ChartId::Overview {
                let w = c.area.width() as f32;
                app().view(|core| core.center(x as f32, w));
            }
        });
        self.root.add_controller(click);

        let drag = gtk::GestureDrag::new();
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(c) = weak.upgrade() {
                c.dragged.set(0.0);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |g, dx, _| {
            let Some(c) = weak.upgrade() else { return };
            let w = c.area.width() as f32;
            if c.id == ChartId::Overview {
                // Dragging along the overview keeps centring the window.
                if let Some((x0, _)) = g.start_point() {
                    app().view(|core| core.center((x0 + dx) as f32, w));
                }
            } else {
                let step = dx - c.dragged.replace(dx);
                if step != 0.0 {
                    app().view(|core| core.pan(step as f32, w));
                }
            }
        });
        self.root.add_controller(drag);

        if !self.time_series() {
            return;
        }

        // Ctrl+wheel zooms around the pointer; a horizontal wheel pans.
        // Plain vertical scrolling goes on to the page.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |ctl, dx, dy| {
            let Some(c) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let w = c.area.width() as f32;
            let x = c.pointer.get().map_or(w / 2.0, |p| p.0);
            let wheel = ctl.unit() == gdk::ScrollUnit::Wheel;
            if ctl
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                if dy != 0.0 {
                    // Smooth scrolling reports points; a wheel, steps.
                    let steps = if wheel { dy } else { dy / WHEEL_PAN };
                    app().view(|core| core.zoom(WHEEL_ZOOM.powf(-steps), x, w));
                }
                return glib::Propagation::Stop;
            }
            if dx != 0.0 && dx.abs() >= dy.abs() {
                let pts = if wheel { dx * WHEEL_PAN } else { dx };
                app().view(|core| core.pan(-pts as f32, w));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.root.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        let weak = Rc::downgrade(self);
        pinch.connect_begin(move |_, _| {
            if let Some(c) = weak.upgrade() {
                c.pinched.set(1.0);
            }
        });
        let weak = Rc::downgrade(self);
        pinch.connect_scale_changed(move |g, scale| {
            let Some(c) = weak.upgrade() else { return };
            let step = scale / c.pinched.replace(scale);
            let w = c.area.width() as f32;
            let x = g.bounding_box_center().map_or(w / 2.0, |p| p.0 as f32);
            if step.is_finite() && step > 0.0 {
                app().view(|core| core.zoom(step, x, w));
            }
        });
        self.root.add_controller(pinch);
    }
}
