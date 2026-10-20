//! The window: headline values, live charts, controls, breakdowns, and the
//! connection info column. Reads the session; never touches the network.

mod breakdown;
mod charts;
mod controls;
mod format;
mod headline;
mod info;
mod log;
mod text;
pub mod theme;
mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, CentralPanel, Panel, Rect, ScrollArea, Ui, Visuals, vec2};

use crate::app::derived::Cache;
use crate::app::session::RunState;
use crate::app::{Controller, Waker, export};
use crate::model::RunMode;
use crate::platform::{self, TrayAction, TrayStatus};
use charts::TimeView;
use charts::timeseries::{Charts, Inputs};
use controls::Action;

/// Runs the egui window until it closes.
pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Raw Speed Test")
            .with_app_id("raw-speed-test")
            .with_inner_size(theme::WINDOW_DEFAULT)
            .with_min_inner_size(theme::WINDOW_MIN),
        centered: true,
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        system_font_fallback: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Raw Speed Test",
        options,
        Box::new(|cc| match SpeedApp::new(cc) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    );
    if let Err(e) = result {
        eprintln!("raw-speed-test: {e}");
        std::process::exit(1);
    }
}

pub struct SpeedApp {
    ctl: Controller,
    text: text::Text,
    cache: Cache,
    view: TimeView,
    charts: Charts,
    boxes: charts::boxplot::Caches,
    info_state: info::State,
    settings_open: bool,
    notice: Option<(String, bool, Instant)>,
    window_ready: bool,
    log_tab: log::Tab,
    /// The menu bar item is shown and closing the window keeps running.
    /// In memory only, like every setting: the app stores nothing.
    menu_bar: bool,
    /// The window is closed and the app lives in the menu bar.
    hidden: bool,
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|s| {
        s.animation_time = 0.0;
        s.scroll_animation = egui::style::ScrollAnimation::none();
        let mut v = Visuals::dark();
        v.panel_fill = theme::BG;
        v.window_fill = theme::SURFACE;
        v.extreme_bg_color = theme::BG;
        v.faint_bg_color = theme::SURFACE;
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_stroke = egui::Stroke::new(theme::STROKE, theme::BORDER);
        v.override_text_color = Some(theme::TEXT);
        v.hyperlink_color = theme::ACCENT;
        v.selection.bg_fill = theme::SURFACE_RAISED;
        v.selection.stroke = egui::Stroke::new(theme::STROKE, theme::ACCENT);
        let r = egui::CornerRadius::same(theme::RADIUS);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = r;
            w.bg_fill = theme::SURFACE;
            w.weak_bg_fill = theme::SURFACE;
            w.bg_stroke = egui::Stroke::new(theme::STROKE, theme::BORDER);
            w.fg_stroke = egui::Stroke::new(theme::STROKE, theme::TEXT);
            w.expansion = 0.0;
        }
        v.widgets.hovered.weak_bg_fill = theme::SURFACE_RAISED;
        v.widgets.active.weak_bg_fill = theme::SURFACE_RAISED;
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(theme::STROKE, theme::TEXT_2);
        s.visuals = v;
        s.spacing.item_spacing = vec2(theme::S8, theme::S8);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.interaction.tooltip_delay = 0.0;
    });
}

impl SpeedApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<SpeedApp, String> {
        platform::current().window_created();
        Self::with_context(&cc.egui_ctx)
    }

    fn with_context(ctx: &egui::Context) -> Result<SpeedApp, String> {
        let ctx = ctx.clone();
        style(&ctx);
        text::install_fonts(&ctx)?;
        let wake_ctx = ctx.clone();
        // Coalesce data-driven frames to at most ~30 per second.
        let waker: Waker = Arc::new(move |after| wake_ctx.request_repaint_after(after));
        Ok(SpeedApp {
            ctl: Controller::new(waker),
            text: text::Text::default(),
            cache: Cache::new(),
            view: TimeView::default(),
            charts: Charts::default(),
            boxes: charts::boxplot::Caches::default(),
            info_state: info::State::default(),
            settings_open: false,
            notice: None,
            window_ready: false,
            log_tab: log::Tab::default(),
            menu_bar: false,
            hidden: false,
        })
    }

    fn export(&mut self) {
        let (json, name) = {
            let s = self.ctl.session();
            let stamp = s
                .wall_start
                .map(|w| {
                    format!(
                        "{}-{}",
                        format::date(w),
                        format::clock(w, false).replace(':', "")
                    )
                })
                .unwrap_or_default();
            (export::to_json(&s), format!("raw-speed-test-{stamp}.json"))
        };
        match platform::current().choose_save_path(&name) {
            Ok(Some(path)) => {
                // Written off the UI thread; the result shows in the status line.
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(std::fs::write(&path, json).map(|_| path));
                });
                match rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(Ok(p)) => self.note(format!("Exported to {}", p.display()), false),
                    Ok(Err(e)) => self.note(format!("Export failed: {e}"), true),
                    Err(_) => self.note("Export failed: write timed out".into(), true),
                }
            }
            Ok(None) => {}
            Err(e) => self.note(format!("Export failed: {e}"), true),
        }
    }

    fn set_menu_bar(&mut self, ctx: &egui::Context, on: bool) {
        self.menu_bar = on;
        let wake_ctx = ctx.clone();
        platform::current().tray_set(on, Arc::new(move || wake_ctx.request_repaint()));
        if on {
            self.update_tray();
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        let plat = platform::current();
        plat.set_dock_icon(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        plat.activate();
        self.hidden = false;
    }

    fn tray_action(&mut self, ctx: &egui::Context, action: TrayAction) {
        let (active, paused) = {
            let s = self.ctl.session();
            (s.active(), s.state == RunState::Paused)
        };
        match action {
            TrayAction::RunOnce | TrayAction::RunContinuously => {
                self.ctl.mode = if action == TrayAction::RunOnce {
                    RunMode::OneShot
                } else {
                    RunMode::Continuous
                };
                self.view = TimeView::default();
                self.ctl.start();
            }
            TrayAction::PauseResume if paused => self.ctl.resume(),
            TrayAction::PauseResume if active => self.ctl.pause(),
            TrayAction::PauseResume => {}
            TrayAction::Stop => self.ctl.stop(),
            TrayAction::ShowWindow => self.show_window(ctx),
            TrayAction::Quit => platform::current().quit(),
        }
    }

    /// Refresh the menu bar item. Returns whether a run is active.
    fn update_tray(&mut self) -> bool {
        let status = {
            let s = self.ctl.session();
            self.cache.update(&s);
            let d = &self.cache.derived;
            let v = |x: Option<f64>, f: fn(f64) -> String, unit: &str| {
                x.map_or_else(|| "—".to_owned(), |x| format!("{} {unit}", f(x)))
            };
            let latency = d.latency.value.map(|x| format!("{} ms", format::ms(x)));
            TrayStatus {
                title: latency.unwrap_or_default(),
                rows: vec![
                    ("Download".into(), v(d.download.value, format::mbps, "Mbps")),
                    ("Upload".into(), v(d.upload.value, format::mbps, "Mbps")),
                    ("Latency".into(), v(d.latency.value, format::ms, "ms")),
                    ("Jitter".into(), v(d.jitter.value, format::ms, "ms")),
                    ("Packet loss".into(), v(d.loss.value, format::pct, "%")),
                ],
                status: controls::status(&s, None).0,
                one_shot_running: s.active() && s.mode == Some(RunMode::OneShot),
                continuous_running: s.active() && s.mode == Some(RunMode::Continuous),
                paused: s.state == RunState::Paused,
            }
        };
        let active = status.one_shot_running || status.continuous_running;
        platform::current().tray_update(&status);
        active
    }

    fn note(&mut self, msg: String, err: bool) {
        self.notice = Some((msg, err, Instant::now()));
    }
}

impl eframe::App for SpeedApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if !self.window_ready {
            platform::current().window_created();
            self.window_ready = true;
        }
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG.to_normalized_gamma_f32()
    }

    /// Runs before every frame, and alone while the window is closed and
    /// the app lives in the menu bar.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let plat = platform::current();
        for action in plat.tray_take_actions() {
            self.tray_action(ctx, action);
        }
        if self.menu_bar && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            plat.set_dock_icon(false);
            self.hidden = true;
        } else if self.hidden && ctx.input(|i| i.viewport().visible()) == Some(true) {
            // Activating the app from the status item can order the window
            // back in; it stays hidden until `Show window`.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if self.menu_bar {
            let active = self.update_tray();
            if active && self.hidden {
                // Status and countdown keep ticking with no window.
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
    }
}

impl SpeedApp {
    fn draw(&mut self, ui: &mut Ui) {
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.2.elapsed() > Duration::from_secs(8))
        {
            self.notice = None;
        }
        let mut actions = Vec::new();
        {
            let SpeedApp {
                ctl,
                text,
                cache,
                view,
                charts,
                boxes,
                info_state,
                settings_open,
                notice,
                log_tab,
                menu_bar,
                ..
            } = self;
            let s = ctl.session();
            cache.update(&s);
            let d = &cache.derived;
            if s.active() {
                // Elapsed time and countdowns tick even without new data.
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
            let run_end = s.now().unwrap_or(1.0);
            let inp = Inputs {
                s: &s,
                text,
                wall0: s.wall_start.unwrap_or(std::time::UNIX_EPOCH),
                run_end,
            };
            Panel::right("info")
                .exact_size(theme::SIDE_WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::BG)
                        .inner_margin(egui::Margin {
                            left: 0,
                            right: theme::S16 as i8,
                            top: theme::S16 as i8,
                            bottom: theme::S16 as i8,
                        }),
                )
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("info-scroll")
                        .show(ui, |ui| {
                            widgets::frame().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 0.0;
                                info::show(ui, text, &s, d, info_state);
                            });
                        });
                });
            CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::BG)
                        .inner_margin(theme::PANEL_MARGIN),
                )
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("main-scroll")
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = theme::S12;
                            headline::show(ui, text, d);
                            let w = ui.available_width();
                            let (r, _) = ui.allocate_exact_size(
                                vec2(w, theme::THROUGHPUT_HEIGHT),
                                egui::Sense::hover(),
                            );
                            charts.throughput(ui, r, &inp, view);
                            let (r, _) = ui.allocate_exact_size(
                                vec2(w, theme::LATENCY_HEIGHT),
                                egui::Sense::hover(),
                            );
                            charts.latency(ui, r, &inp, view);
                            let (r, _) = ui.allocate_exact_size(
                                vec2(w, theme::OVERVIEW_HEIGHT),
                                egui::Sense::hover(),
                            );
                            charts.overview(ui, Rect::from_min_size(r.min, r.size()), &inp, view);
                            let notice = notice.as_ref().map(|(m, e, _)| (m.as_str(), *e));
                            controls::show(ui, text, &s, ctl.mode, notice, &mut actions);
                            if *settings_open {
                                let menu_bar =
                                    platform::current().tray_supported().then_some(*menu_bar);
                                let (cfg, bar) =
                                    controls::settings(ui, text, &ctl.config, d, menu_bar);
                                if let Some(cfg) = cfg {
                                    actions.push(Action::Config(cfg));
                                }
                                if let Some(on) = bar {
                                    actions.push(Action::MenuBar(on));
                                }
                            }
                            breakdown::show(ui, text, &s, d, boxes);
                            widgets::frame().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 0.0;
                                log::show(ui, text, &s, log_tab);
                            });
                        });
                });
        }
        for a in actions {
            match a {
                Action::Start => {
                    self.view = TimeView::default();
                    self.ctl.start();
                }
                Action::Stop => self.ctl.stop(),
                Action::Pause => self.ctl.pause(),
                Action::Resume => self.ctl.resume(),
                Action::Reset => {
                    self.view = TimeView::default();
                    self.ctl.reset();
                }
                Action::Export => self.export(),
                Action::Mode(m) => self.ctl.mode = m,
                Action::Config(c) => {
                    self.ctl.config = c;
                    if self.ctl.session().active() && self.ctl.mode == RunMode::Continuous {
                        self.ctl.apply_config();
                    }
                }
                Action::ToggleSettings => self.settings_open = !self.settings_open,
                Action::MenuBar(on) => self.set_menu_bar(ui.ctx(), on),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use eframe::egui::{Event, Pos2, RawInput, Rect, pos2, vec2};

    use super::*;
    use crate::app::session::synthetic;

    fn frame(app: &mut SpeedApp, ctx: &egui::Context, hover: Option<Pos2>) -> usize {
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, theme::WINDOW_DEFAULT)),
            ..RawInput::default()
        };
        if let Some(p) = hover {
            input.events.push(Event::PointerMoved(p));
        }
        let out = ctx.run_ui(input, |ui| app.draw(ui));
        let prims = ctx.tessellate(out.shapes, 2.0);
        prims.len()
    }

    /// Four simulated days of continuous mode at default settings: the
    /// session stays within its buffers and a full frame (layout, charts,
    /// tessellation) stays cheap. Run with
    /// `cargo test --release soak -- --ignored --nocapture`.
    #[test]
    #[ignore = "slow in debug builds"]
    fn soak_multi_day_session_renders_quickly() {
        let ctx = egui::Context::default();
        let mut app = SpeedApp::with_context(&ctx).expect("system font");
        let t = Instant::now();
        let synth = synthetic(4.0 * 86_400.0);
        eprintln!("synthesized in {:?}", t.elapsed());
        assert_eq!(synth.latency.raw.len(), crate::app::session::RAW_LATENCY);
        assert!(!synth.loss.raw_complete());
        *app.ctl.session() = synth;
        // Warm up font atlas and caches.
        for _ in 0..3 {
            frame(&mut app, &ctx, None);
        }
        let n = 30;
        let t = Instant::now();
        for i in 0..n {
            // Hover across the throughput and latency charts.
            let x = 100.0 + (i as f32) * 25.0;
            frame(
                &mut app,
                &ctx,
                Some(pos2(x, 260.0 + (i % 2) as f32 * 150.0)),
            );
        }
        let per = t.elapsed() / n;
        eprintln!("frame with hover over 4 days of data: {per:?}");
        // Zoom into the last hour (raw data) and render again.
        app.view.zoom(96.0, 4.0 * 86_400.0, 4.0 * 86_400.0);
        let t = Instant::now();
        for i in 0..n {
            frame(&mut app, &ctx, Some(pos2(300.0 + i as f32 * 10.0, 260.0)));
        }
        let per_zoomed = t.elapsed() / n;
        eprintln!("frame zoomed to the last hour: {per_zoomed:?}");
        let _ = vec2(0.0, 0.0);
        assert!(per.as_millis() < 50, "{per:?}");
        assert!(per_zoomed.as_millis() < 50, "{per_zoomed:?}");
    }

    /// Text of every galley painted in a frame with the pointer at `p`.
    fn texts_at(app: &mut SpeedApp, ctx: &egui::Context, p: Pos2) -> Vec<String> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, theme::WINDOW_DEFAULT)),
            events: vec![Event::PointerMoved(p)],
            ..RawInput::default()
        };
        let out = ctx.run_ui(input, |ui| app.draw(ui));
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Sweeping the pointer over the window finds an exact-value tooltip on
    /// every kind of plotted element and on the headline values.
    #[test]
    #[ignore = "needs the system UI font; run with --ignored"]
    fn every_plot_has_hover_values() {
        let ctx = egui::Context::default();
        let mut app = SpeedApp::with_context(&ctx).expect("system font");
        *app.ctl.session() = synthetic(1800.0);
        frame(&mut app, &ctx, None);
        let wanted = [
            "Throughput",   // throughput bin tooltip
            "Edge TCP RTT", // latency probe tooltip
            "Round trip",   // gateway or UDP probe tooltip
            "Samples",      // box plot summary tooltip
            "Median",       // headline tooltip
            "Elapsed",      // overview strip tooltip
        ];
        let mut found = [false; 6];
        let size = theme::WINDOW_DEFAULT;
        let mut y = 4.0;
        while y < size.y {
            let mut x = 4.0;
            while x < size.x - theme::SIDE_WIDTH {
                let texts = texts_at(&mut app, &ctx, pos2(x, y));
                for (i, w) in wanted.iter().enumerate() {
                    if texts.iter().any(|t| t == w) {
                        found[i] = true;
                    }
                }
                x += 9.0;
            }
            y += 6.0;
        }
        for (w, f) in wanted.iter().zip(found) {
            assert!(f, "no tooltip containing {w:?}");
        }
    }
}
