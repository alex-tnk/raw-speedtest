//! Linux shell: GTK 4 + libadwaita on the shared view layer. The window is
//! an `AdwApplicationWindow` with a header bar (run controls and status),
//! the headline values, charts, breakdowns and log in a scrolled column,
//! and connection info in a sidebar. Charts are drawing areas that paint
//! the core's scenes with Cairo.
//!
//! Everything runs on the GTK main thread. The core's waker, called from
//! its collector thread, schedules one coalesced update on the main loop;
//! each update compares the new frame with the last one and touches only
//! the widgets whose part changed.

mod chart;
mod draw;
mod log;
mod panels;
mod settings;
mod style;
mod tip;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::model::RunMode;
use crate::view::chart::Hover;
use crate::view::core::{ChartId, Core};
use crate::view::frame::{Controls, Frame, Setting};
use crate::view::scene::TextSize;
use chart::Chart;
use panels::{Columns, Header, Headlines, Info, S4, S12, S16};

const APP_ID: &str = "io.github.alex-tnk.raw-speed-test";
const TITLE: &str = "Raw Speed Test";

/// Window sizes in points.
const DEFAULT_SIZE: (i32, i32) = (1440, 920);
const MIN_SIZE: (i32, i32) = (1000, 700);
/// Below this width the connection sidebar overlays the content.
const SIDEBAR_BREAKPOINT: &str = "max-width: 1200sp";
const SIDEBAR_WIDTH: (f64, f64) = (280.0, 340.0);

/// Chart heights in points.
const THROUGHPUT_HEIGHT: f32 = 196.0;
const LATENCY_HEIGHT: f32 = 148.0;
const OVERVIEW_HEIGHT: f32 = 32.0;

thread_local! {
    static APP: OnceCell<Rc<App>> = const { OnceCell::new() };
}

/// The shell's state. Signal handlers and draw functions reach it here;
/// they run only on the main thread, after the window is built.
fn app() -> Rc<App> {
    try_app().expect("the window exists")
}

fn try_app() -> Option<Rc<App>> {
    APP.with(|a| a.get().cloned())
}

/// Set while an update is queued on the main loop.
static PENDING: AtomicBool = AtomicBool::new(false);

/// The core's waker: one update per burst of wakes, after the delay the
/// first wake asks for.
fn waker() -> crate::app::Waker {
    Arc::new(|delay| {
        if !PENDING.swap(true, Ordering::AcqRel) {
            glib::timeout_add_once(delay, || {
                PENDING.store(false, Ordering::Release);
                if let Some(a) = try_app() {
                    a.update();
                }
            });
        }
    })
}

pub fn run() {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|_| {
        // GTK's recently-used list is a file in the user's data directory.
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_recent_files_enabled(false);
        }
    });
    application.connect_activate(activate);
    // Termination signals quit through the main loop, like closing the
    // window.
    crate::platform::set_quit_hook(Box::new(|| {
        glib::MainContext::default().invoke(|| {
            if let Some(a) = gio::Application::default() {
                a.quit();
            }
        });
    }));
    // No command-line options: GTK must not parse the process arguments.
    application.run_with_args::<&str>(&[]);
}

struct App {
    core: RefCell<Core>,
    window: adw::ApplicationWindow,
    header: Header,
    headlines: RefCell<Headlines>,
    legends: [gtk::Box; 2],
    throughput: Rc<Chart>,
    latency: Rc<Chart>,
    overview: Rc<Chart>,
    columns: RefCell<Columns>,
    info: RefCell<Info>,
    logs: Rc<log::Logs>,
    settings: RefCell<Option<settings::SettingsUi>>,
    tips: Rc<tip::Layer>,
    /// Zero-size widgets coloured with the window background and the
    /// accent, read back for Cairo.
    probes: [gtk::Box; 2],
    css: gtk::CssProvider,
    last: RefCell<Option<Frame>>,
    /// Set while the app changes its own widgets, so their signals are
    /// not taken as user input.
    updating: Cell<bool>,
    tick: RefCell<Option<glib::SourceId>>,
    hovered: RefCell<Option<Rc<Chart>>>,
    /// Height of a line of small chart text.
    label_h: Cell<f32>,
}

fn activate(application: &adw::Application) {
    if let Some(a) = try_app() {
        a.window.present();
        return;
    }
    let header = Header::new();
    let headlines = Headlines::new();
    let legends = [0, 1].map(|_| gtk::Box::new(gtk::Orientation::Horizontal, S16));
    let throughput = Chart::new(ChartId::Throughput, THROUGHPUT_HEIGHT);
    let latency = Chart::new(ChartId::Latency, LATENCY_HEIGHT);
    let overview = Chart::new(ChartId::Overview, OVERVIEW_HEIGHT);
    let columns = Columns::new();
    let info = Info::new();
    let logs = log::Logs::new(|tab, run, lo, hi| {
        try_app()
            .and_then(|a| {
                let core = a.core.try_borrow().ok()?;
                Some(core.with_session(|s| log::fetch(s, tab, run, lo, hi)))
            })
            .unwrap_or_default()
    });

    let charts = gtk::Box::new(gtk::Orientation::Vertical, S4);
    charts.append(&legends[0]);
    charts.append(&throughput.root);
    legends[1].set_margin_top(S12);
    charts.append(&legends[1]);
    charts.append(&latency.root);
    overview.root.set_margin_top(S4);
    charts.append(&overview.root);
    let main = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(S16)
        .margin_top(S16)
        .margin_bottom(S16)
        .margin_start(S16)
        .margin_end(S16)
        .build();
    main.append(&headlines.root);
    main.append(&charts);
    main.append(&columns.root);
    main.append(&logs.root);
    let content = gtk::ScrolledWindow::builder()
        .child(&main)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let sidebar = gtk::ScrolledWindow::builder()
        .child(&info.root)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let split = adw::OverlaySplitView::builder()
        .content(&content)
        .sidebar(&sidebar)
        .sidebar_position(gtk::PackType::End)
        .min_sidebar_width(SIDEBAR_WIDTH.0)
        .max_sidebar_width(SIDEBAR_WIDTH.1)
        .build();
    split
        .bind_property("show-sidebar", &header.sidebar, "active")
        .bidirectional()
        .sync_create()
        .build();
    let tips = tip::Layer::new(&split);
    let probes = ["rst-fg-background", "rst-fg-accent"].map(|class| {
        gtk::Box::builder()
            .css_classes([class])
            .can_target(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .build()
    });
    for p in &probes {
        tips.overlay.add_overlay(p);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header.bar);
    toolbar.set_content(Some(&tips.overlay));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title(TITLE)
        .default_width(DEFAULT_SIZE.0)
        .default_height(DEFAULT_SIZE.1)
        .width_request(MIN_SIZE.0)
        .height_request(MIN_SIZE.1)
        .content(&toolbar)
        .build();
    match adw::BreakpointCondition::parse(SIDEBAR_BREAKPOINT) {
        Ok(cond) => {
            let bp = adw::Breakpoint::new(cond);
            bp.add_setter(&split, "collapsed", Some(&true.to_value()));
            bp.add_setter(&split, "show-sidebar", Some(&false.to_value()));
            window.add_breakpoint(bp);
        }
        Err(e) => glib::g_warning!("raw-speed-test", "breakpoint: {e}"),
    }

    let style = adw::StyleManager::default();
    let css = gtk::CssProvider::new();
    css.load_from_string(&style::css(style.is_dark()));
    if let Some(d) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &d,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let a = Rc::new(App {
        core: RefCell::new(Core::new(waker())),
        window,
        header,
        headlines: RefCell::new(headlines),
        legends,
        throughput,
        latency,
        overview,
        columns: RefCell::new(columns),
        info: RefCell::new(info),
        logs,
        settings: RefCell::new(None),
        tips,
        probes,
        css,
        last: RefCell::new(None),
        updating: Cell::new(false),
        tick: RefCell::new(None),
        hovered: RefCell::new(None),
        label_h: Cell::new(0.0),
    });
    APP.with(|cell| {
        let _ = cell.set(a.clone());
    });

    style.connect_dark_notify(|sm| {
        let a = app();
        a.css.load_from_string(&style::css(sm.is_dark()));
        a.redraw_all();
    });
    style.connect_high_contrast_notify(|_| app().redraw_all());
    let h = &a.header;
    h.one_shot.connect_toggled(|b| {
        if b.is_active() {
            app().set_mode(RunMode::OneShot);
        }
    });
    h.continuous.connect_toggled(|b| {
        if b.is_active() {
            app().set_mode(RunMode::Continuous);
        }
    });
    h.start.connect_clicked(|_| {
        let a = app();
        let active = a.controls().is_some_and(|c| c.active);
        a.act(|core| if active { core.stop() } else { core.start() });
    });
    h.pause.connect_clicked(|_| {
        let a = app();
        let paused = a.controls().is_some_and(|c| c.paused);
        a.act(|core| if paused { core.resume() } else { core.pause() });
    });
    h.reset.connect_clicked(|_| app().act(Core::reset));
    h.export.connect_clicked(|_| app().export());
    h.settings.connect_clicked(|_| app().open_settings());

    a.update();
    a.window.present();
}

impl App {
    fn controls(&self) -> Option<Controls> {
        self.last.borrow().as_ref().map(|f| f.controls)
    }

    /// Runs a user command on the core and shows its effect.
    fn act(&self, f: impl FnOnce(&mut Core)) {
        if self.updating.get() {
            return;
        }
        f(&mut self.core.borrow_mut());
        self.update();
    }

    fn set_mode(&self, mode: RunMode) {
        self.act(|core| core.set_mode(mode));
    }

    fn set_setting(&self, id: Setting, index: usize) {
        self.act(|core| core.set(id, index));
    }

    /// Changes the shared time window and redraws the time charts.
    fn view(&self, f: impl FnOnce(&mut Core)) {
        f(&mut self.core.borrow_mut());
        for c in [&self.throughput, &self.latency, &self.overview] {
            self.refresh_chart(c);
        }
        self.rehover();
    }

    /// Applies a new frame: only the parts that changed are touched.
    fn update(&self) {
        if self.updating.replace(true) {
            return;
        }
        let f = self.core.borrow_mut().frame();
        let prev = self.last.borrow_mut().take();
        let p = prev.as_ref();
        if p.map(|p| &p.headline) != Some(&f.headline) {
            self.headlines.borrow_mut().update(&f.headline);
        }
        if p.map(|p| &p.status) != Some(&f.status) {
            self.header.status(&f.status);
        }
        if p.map(|p| p.progress) != Some(f.progress) {
            self.header.progress(f.progress);
        }
        if p.map(|p| &p.elapsed) != Some(&f.elapsed) {
            self.header.elapsed(f.elapsed.as_deref());
        }
        if p.map(|p| p.controls) != Some(f.controls) {
            self.header.controls(&f.controls);
        }
        if p.map(|p| &p.legends) != Some(&f.legends) {
            panels::legend(&self.legends[0], &f.legends.throughput);
            panels::legend(&self.legends[1], &f.legends.latency);
        }
        if p.map(|p| &p.columns) != Some(&f.columns) {
            self.columns.borrow_mut().update(&f.columns);
        }
        if p.map(|p| (&p.info, p.rssi)) != Some((&f.info, f.rssi)) {
            self.info.borrow_mut().update(&f.info, f.rssi);
        }
        if p.map(|p| &p.settings) != Some(&f.settings)
            && let Some(s) = &*self.settings.borrow()
        {
            s.update(&f.settings);
        }
        if p.map(|p| p.log) != Some(f.log) {
            self.logs
                .counts([f.log.requests, f.log.connections, f.log.errors]);
        }
        *self.last.borrow_mut() = Some(f);
        self.updating.set(false);

        self.sync_logs();
        self.refresh_charts();
        self.rehover();
        self.schedule_tick();
    }

    /// Asks for an update when the core wants one without new data
    /// (elapsed time, countdowns, the end of an export notice).
    fn schedule_tick(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
        let Some(after) = self.core.borrow().next_tick() else {
            return;
        };
        let id = glib::timeout_add_local_once(after, || {
            if let Some(a) = try_app() {
                // The source is done once this runs.
                a.tick.borrow_mut().take();
                a.update();
            }
        });
        *self.tick.borrow_mut() = Some(id);
    }

    fn sync_logs(&self) {
        let counts: Vec<log::Counts> = self.core.borrow().with_session(|s| {
            self.logs
                .tabs
                .iter()
                .map(|t| log::counts(s, t.tab))
                .collect()
        });
        for (t, c) in self.logs.tabs.iter().zip(counts) {
            t.sync(c);
        }
    }

    fn charts(&self) -> Vec<Rc<Chart>> {
        let mut v = vec![
            self.throughput.clone(),
            self.latency.clone(),
            self.overview.clone(),
        ];
        v.extend(self.columns.borrow().charts().cloned());
        v.extend(self.info.borrow().rssi().cloned());
        v
    }

    fn refresh_charts(&self) {
        for c in self.charts() {
            self.refresh_chart(&c);
        }
    }

    /// Hands chart `c` its scene at its current size; it redraws only if
    /// the scene changed.
    fn refresh_chart(&self, c: &Chart) {
        let Some((w, h)) = c.size() else {
            return;
        };
        let label_h = self.label_h(c.area());
        match self.core.try_borrow_mut() {
            Ok(mut core) => c.set_scene(core.scene(c.id, w, h, label_h)),
            // Resized while the core is in use: catch up right after.
            Err(_) => {
                glib::idle_add_local_once(|| app().refresh_charts());
            }
        }
    }

    fn label_h(&self, widget: &gtk::DrawingArea) -> f32 {
        if self.label_h.get() <= 0.0 {
            let l = draw::layout(widget, draw::text_px(TextSize::Small));
            l.set_text("0");
            self.label_h.set(l.pixel_size().1 as f32);
        }
        self.label_h.get()
    }

    fn redraw_all(&self) {
        for c in self.charts() {
            c.redraw();
        }
    }

    pub fn palette(&self, area: &gtk::DrawingArea) -> style::Palette {
        style::Palette {
            fg: area.color(),
            background: self.probes[0].color(),
            accent: self.probes[1].color(),
            dark: adw::StyleManager::default().is_dark(),
        }
    }

    /// Chart `c`'s pointer moved or left: crosshair, highlight and tip.
    fn hover(&self, c: &Rc<Chart>) {
        let pointer = c.pointer();
        let h = match (pointer, c.size()) {
            (Some((x, y)), Some((w, ht))) => self.core.borrow_mut().hover(c.id, w, ht, x, y),
            _ => Hover::default(),
        };
        let (tip, anchor) = (h.tip.clone(), h.anchor);
        let changed = c.set_hover(h);
        let current = self
            .hovered
            .borrow()
            .as_ref()
            .is_some_and(|x| Rc::ptr_eq(x, c));
        if pointer.is_none() {
            if current {
                self.hovered.replace(None);
                self.tips.hide();
            }
            return;
        }
        if !current && let Some(old) = self.hovered.replace(Some(c.clone())) {
            old.set_hover(Hover::default());
        }
        if changed || !current {
            match tip {
                Some(t) => self.tips.show(&t, c.area(), (anchor.x, anchor.y)),
                None => self.tips.hide(),
            }
        }
    }

    /// Hover again after the data or the time window changed under a
    /// pointer that did not move.
    fn rehover(&self) {
        let Some(c) = self.hovered.borrow().clone() else {
            return;
        };
        if c.root.is_mapped() {
            self.hover(&c);
        } else {
            // Its column was rebuilt.
            self.hovered.replace(None);
            self.tips.hide();
        }
    }

    fn open_settings(&self) {
        if let Some(s) = &*self.settings.borrow() {
            s.dialog.present(Some(&self.window));
            return;
        }
        let Some(frame) = self.last.borrow().as_ref().map(|f| f.settings.clone()) else {
            return;
        };
        let ui = settings::SettingsUi::new(&frame);
        ui.dialog.connect_closed(|_| {
            app().settings.borrow_mut().take();
        });
        ui.dialog.present(Some(&self.window));
        *self.settings.borrow_mut() = Some(ui);
    }

    /// Asks where to save, then writes the export off the main thread;
    /// the result shows in the status line.
    fn export(&self) {
        let name = self
            .last
            .borrow()
            .as_ref()
            .map(|f| f.export_name.clone())
            .unwrap_or_default();
        let dialog = gtk::FileDialog::builder()
            .title("Export")
            .accept_label("Export")
            .initial_name(name)
            .modal(true)
            .build();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, |res| {
            // Cancelled, or a location without a local path (the export
            // is written with the standard library).
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            app().act(|core| core.export(path));
        });
    }
}
