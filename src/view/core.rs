//! What a shell talks to: owns the controller, the derived-statistics
//! cache, the shared time window and per-chart scene caches. Every method
//! is called from the shell's UI thread; the session lock is held only
//! while a frame, scene, hover or page of log rows is built.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use super::chart::boxplot::{self, BoxId};
use super::chart::series::{self, Charts, Inputs};
use super::chart::{Hover, TimeView, rssi, time_plot};
use super::frame::{self, Frame, Setting};
use super::log::{self, Tab};
use super::scene::{Point, Scene};
use crate::app::derived::Cache;
use crate::app::session::Session;
use crate::app::{Controller, Waker, export};
use crate::model::{Config, RunMode};

/// How long an export notice stays in the status line.
const NOTICE: Duration = Duration::from_secs(8);
/// Elapsed time and countdowns (whole seconds) tick this often while a
/// run is active.
const TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChartId {
    Throughput,
    Latency,
    Overview,
    Box(BoxId),
    Rssi,
}

impl ChartId {
    /// From the FFI encoding: kind, plus the box plot for kind 3.
    pub fn from_raw(kind: u8, index: u8) -> Option<ChartId> {
        Some(match kind {
            0 => ChartId::Throughput,
            1 => ChartId::Latency,
            2 => ChartId::Overview,
            3 => ChartId::Box(BoxId::from_u8(index)?),
            4 => ChartId::Rssi,
            _ => return None,
        })
    }
}

type ExportResult = Result<PathBuf, String>;

/// What a cached scene was built from.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    /// Changes when the data the chart draws changes.
    data: u64,
    size: [u32; 3],
    /// The time window's settings (span, end).
    view: [u64; 2],
    /// The time range drawn, which creeps with the clock while following.
    range: (f64, f64),
}

impl Key {
    const NONE: Key = Key {
        data: u64::MAX,
        size: [0; 3],
        view: [0; 2],
        range: (f64::NAN, f64::NAN),
    };

    /// Whether a scene built for `self` must be rebuilt for `new`. The
    /// time range may creep by less than a point (`secs_per_pt`) before it
    /// shows, so a chart following live data isn't redrawn for nothing.
    fn stale(&self, new: &Key, secs_per_pt: f64) -> bool {
        self.data != new.data
            || self.size != new.size
            || self.view != new.view
            || !((self.range.0 - new.range.0).abs() < secs_per_pt
                && (self.range.1 - new.range.1).abs() < secs_per_pt)
    }
}

/// Mixes counters into one key.
fn mix(parts: &[u64]) -> u64 {
    parts.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, p| {
        (h ^ p).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Distinguishes runs, so a reset or a new run always rebuilds.
fn run_id(s: &Session) -> u64 {
    s.wall_start
        .and_then(|w| w.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

/// What each chart draws, as a key that changes only when that data does.
fn data_key(id: ChartId, s: &Session, run_end: f64) -> u64 {
    let ring = |len: usize, evicted: u64| [len as u64, evicted];
    match id {
        ChartId::Throughput => {
            let [a, b] = ring(s.throughput[0].raw.len(), s.throughput[0].raw.evicted());
            let [c, d] = ring(s.throughput[1].raw.len(), s.throughput[1].raw.evicted());
            let [e, f] = ring(s.requests.len(), s.requests.evicted());
            mix(&[a, b, c, d, e, f, run_id(s)])
        }
        ChartId::Latency => {
            let paused = series::Charts::latency_ticks_with_clock(s);
            mix(&[
                s.latency.raw.len() as u64,
                s.latency.raw.evicted(),
                s.gateway.raw.len() as u64,
                s.gateway.raw.evicted(),
                s.loss.raw.len() as u64,
                s.loss.raw.evicted(),
                s.requests.len() as u64,
                s.pauses.len() as u64,
                s.config.continuous.latency_interval.as_millis() as u64,
                // An ongoing pause band grows with the clock, not the data.
                if paused { run_end as u64 } else { u64::MAX },
                run_id(s),
            ])
        }
        // Rebuilt at most once per second of run time or 64 changes.
        ChartId::Overview => mix(&[s.version / 64, run_end as u64, s.state as u64]),
        // A box plot changes only when its own samples do.
        ChartId::Box(b) => {
            let [n, evicted] = boxplot::data_key(b, s);
            mix(&[n, evicted, run_id(s)])
        }
        ChartId::Rssi => mix(&[
            s.wifi_samples.raw.len() as u64,
            s.wifi_samples.raw.evicted(),
            run_id(s),
        ]),
    }
}

pub struct Core {
    ctl: Controller,
    wake: Waker,
    cache: Cache,
    view: TimeView,
    notice: Option<(String, bool, Instant)>,
    exports: (Sender<ExportResult>, Receiver<ExportResult>),
    charts: Charts,
    /// Per chart: what the scene was built from, the scene, and a counter
    /// bumped on every rebuild.
    scenes: HashMap<ChartId, (Key, Scene, u64)>,
    bits: Vec<u64>,
    pts: Vec<Point>,
    breaks: Vec<usize>,
}

impl Core {
    pub fn new(wake: Waker) -> Core {
        Core {
            ctl: Controller::new(wake.clone()),
            wake,
            cache: Cache::default(),
            view: TimeView::default(),
            notice: None,
            exports: channel(),
            charts: Charts::default(),
            scenes: HashMap::new(),
            bits: Vec::new(),
            pts: Vec::new(),
            breaks: Vec::new(),
        }
    }

    /// Everything outside the charts.
    pub fn frame(&mut self) -> Frame {
        while let Ok(r) = self.exports.1.try_recv() {
            let (msg, err) = match r {
                Ok(p) => (format!("Exported to {}", p.display()), false),
                Err(e) => (format!("Export failed: {e}"), true),
            };
            self.notice = Some((msg, err, Instant::now()));
        }
        if self.notice.as_ref().is_some_and(|n| n.2.elapsed() > NOTICE) {
            self.notice = None;
        }
        let s = self.ctl.session();
        self.cache.update(&s);
        let notice = self.notice.as_ref().map(|(m, e, _)| (m.as_str(), *e));
        frame::build(
            &s,
            &self.cache.derived,
            self.ctl.mode,
            &self.ctl.config,
            notice,
        )
    }

    /// When the shell should ask for a frame again without new data:
    /// elapsed time and countdowns while a run is active, and the end of
    /// an export notice.
    pub fn next_tick(&self) -> Option<Duration> {
        let active = self.ctl.session().active();
        let notice = self
            .notice
            .as_ref()
            .map(|n| NOTICE.saturating_sub(n.2.elapsed()));
        match (active.then_some(TICK), notice) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The scene of chart `id` at `w`×`h` points; `label_h` is the height
    /// of a small text line, used to keep percentile labels apart.
    pub fn scene(&mut self, id: ChartId, w: f32, h: f32, label_h: f32) -> &Scene {
        let Core {
            ctl,
            cache,
            view,
            charts,
            scenes,
            bits,
            pts,
            breaks,
            ..
        } = self;
        let s = ctl.session();
        let run_end = s.now().unwrap_or(1.0);
        let (t0, t1) = view.range(run_end);
        let time = matches!(id, ChartId::Throughput | ChartId::Latency);
        let key = Key {
            data: data_key(id, &s, run_end),
            size: [w.to_bits(), h.to_bits(), label_h.to_bits()],
            view: match id {
                ChartId::Throughput | ChartId::Latency | ChartId::Overview => [
                    view.span.unwrap_or(-1.0).to_bits(),
                    view.end.unwrap_or(-1.0).to_bits(),
                ],
                _ => [0, 0],
            },
            range: if time { (t0, t1) } else { (0.0, 0.0) },
        };
        let secs_per_pt = (t1 - t0) / f64::from(time_plot(w, h).width().max(1.0));
        let entry = scenes
            .entry(id)
            .or_insert_with(|| (Key::NONE, Scene::default(), 0));
        if entry
            .0
            .stale(&key, if time { secs_per_pt } else { f64::INFINITY })
        {
            let inp = Inputs {
                s: &s,
                wall0: s.wall_start.unwrap_or(SystemTime::UNIX_EPOCH),
                run_end,
            };
            let sc = &mut entry.1;
            match id {
                ChartId::Throughput => charts.throughput(&inp, view, w, h, label_h, sc),
                ChartId::Latency => charts.latency(&inp, view, w, h, label_h, sc),
                ChartId::Overview => charts.overview(&inp, view, w, h, sc),
                ChartId::Box(b) => {
                    cache.update(&s);
                    boxplot::scene(b, &s, &cache.derived, w, h, sc, bits);
                }
                ChartId::Rssi => rssi::scene(&s, w, h, sc, pts, breaks),
            }
            entry.0 = key;
            entry.2 += 1;
        }
        &entry.1
    }

    /// Bumped each time the scene of `id` is rebuilt, so a shell can keep
    /// its own drawing objects until then. 0 before the first build.
    pub fn scene_generation(&self, id: ChartId) -> u64 {
        self.scenes.get(&id).map_or(0, |e| e.2)
    }

    /// What hovering chart `id` at chart-local `(x, y)` shows.
    pub fn hover(&mut self, id: ChartId, w: f32, h: f32, x: f32, y: f32) -> Hover {
        let s = self.ctl.session();
        let hp = Point { x, y };
        let inp = Inputs {
            s: &s,
            wall0: s.wall_start.unwrap_or(SystemTime::UNIX_EPOCH),
            run_end: s.now().unwrap_or(1.0),
        };
        match id {
            ChartId::Throughput => self.charts.throughput_hover(&inp, &self.view, w, h, hp),
            ChartId::Latency => self.charts.latency_hover(&inp, &self.view, w, h, hp),
            ChartId::Overview => Charts::overview_hover(&inp, w, h, hp),
            ChartId::Box(b) => {
                self.cache.update(&s);
                boxplot::hover(b, &s, &self.cache.derived, w, h, hp)
            }
            ChartId::Rssi => rssi::hover(&s, w, h, hp),
        }
    }

    fn run_end(&self) -> f64 {
        self.ctl.session().now().unwrap_or(1.0)
    }

    /// Run time at chart-local `x` in a time-series chart `w` wide.
    fn time_at(&self, x: f32, w: f32) -> f64 {
        let (t0, t1) = self.view.range(self.run_end());
        let plot = time_plot(w, 1.0);
        t0 + f64::from((x - plot.x0) / plot.width().max(1.0)) * (t1 - t0)
    }

    /// Zoom the time window by `factor` (> 1 zooms in) around chart-local
    /// `x` of a time-series chart `w` wide.
    pub fn zoom(&mut self, factor: f64, x: f32, w: f32) {
        let at = self.time_at(x, w);
        let run_end = self.run_end();
        self.view.zoom(factor, at, run_end);
    }

    /// Pan by `dx` points (drag right moves back in time).
    pub fn pan(&mut self, dx: f32, w: f32) {
        let run_end = self.run_end();
        let (t0, t1) = self.view.range(run_end);
        let secs_per_pt = (t1 - t0) / f64::from(time_plot(w, 1.0).width().max(1.0));
        self.view.pan(-f64::from(dx) * secs_per_pt, run_end);
    }

    /// Center the window on chart-local `x` of the overview strip.
    pub fn center(&mut self, x: f32, w: f32) {
        let run_end = self.run_end();
        let plot = time_plot(w, 1.0);
        let t = f64::from((x - plot.x0) / plot.width().max(1.0)) * run_end.max(1.0);
        self.view.center(t, run_end);
    }

    pub fn reset_view(&mut self) {
        self.view = TimeView::default();
    }

    pub fn zoomed(&self) -> bool {
        self.view.span.is_some()
    }

    pub fn mode(&self) -> RunMode {
        self.ctl.mode
    }

    /// Takes effect for the next run.
    pub fn set_mode(&mut self, mode: RunMode) {
        if !self.ctl.session().active() {
            self.ctl.mode = mode;
        }
    }

    pub fn config(&self) -> Config {
        self.ctl.config
    }

    /// Choose option `index` of a setting; a continuous run picks it up.
    pub fn set(&mut self, setting: Setting, index: usize) {
        let Some(cfg) = setting.apply(&self.ctl.config, index) else {
            return;
        };
        self.ctl.config = cfg;
        if self.ctl.session().active() && self.ctl.mode == RunMode::Continuous {
            self.ctl.apply_config();
        }
    }

    pub fn start(&mut self) {
        self.view = TimeView::default();
        self.ctl.start();
    }

    /// Start a run in `mode`, restarting one that is active.
    pub fn start_mode(&mut self, mode: RunMode) {
        self.ctl.mode = mode;
        self.start();
    }

    pub fn stop(&mut self) {
        self.ctl.stop();
    }

    pub fn pause(&mut self) {
        self.ctl.pause();
    }

    pub fn resume(&mut self) {
        self.ctl.resume();
    }

    pub fn reset(&mut self) {
        self.view = TimeView::default();
        self.ctl.reset();
    }

    pub fn log_len(&self, tab: Tab) -> usize {
        log::len(&self.ctl.session(), tab)
    }

    pub fn log_rows(&self, tab: Tab, first: usize, count: usize) -> Vec<log::Row> {
        log::rows(&self.ctl.session(), tab, first, count)
    }

    /// Write every sample as JSON to `path` off the UI thread; the result
    /// shows in the status line.
    pub fn export(&mut self, path: PathBuf) {
        let shared = self.ctl.shared();
        let tx = self.exports.0.clone();
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                let json = {
                    let s = shared.lock().unwrap_or_else(|e| e.into_inner());
                    export::to_json(&s)
                };
                let r = std::fs::write(&path, json)
                    .map(|_| path)
                    .map_err(|e| e.to_string());
                let _ = tx.send(r);
                wake(Duration::ZERO);
            });
        if let Err(e) = spawned {
            let _ = self.exports.0.send(Err(e.to_string()));
        }
    }

    /// Run `f` with the session (tests, and shells' own summaries).
    pub fn with_session<T>(&self, f: impl FnOnce(&Session) -> T) -> T {
        f(&self.ctl.session())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::chart::boxplot::BoxId;
    use crate::view::scene::Prim;
    use std::sync::Arc;

    fn core_with(secs: f64) -> Core {
        let core = Core::new(Arc::new(|_| {}));
        *core.ctl.session() = crate::app::session::synthetic(secs);
        core
    }

    const W: f32 = 1000.0;

    fn charts() -> Vec<(ChartId, f32)> {
        let mut v = vec![
            (ChartId::Throughput, 196.0),
            (ChartId::Latency, 148.0),
            (ChartId::Overview, 32.0),
            (ChartId::Rssi, rssi::HEIGHT),
        ];
        v.extend(BoxId::ALL.map(|b| (ChartId::Box(b), boxplot::height(3))));
        v
    }

    fn check_scene(id: ChartId, sc: &Scene, w: f32, h: f32) {
        for p in &sc.pts {
            assert!(
                p.x.is_finite() && p.y.is_finite(),
                "{id:?} non-finite point"
            );
            assert!(
                p.x >= -1.0 && p.x <= w + 1.0 && p.y >= -1.0 && p.y <= h + 1.0,
                "{id:?} point {p:?} outside {w}x{h}"
            );
        }
        for prim in &sc.prims {
            match prim {
                Prim::Line { range, .. } | Prim::Area { range, .. } | Prim::Dots { range, .. } => {
                    assert!(range.end as usize <= sc.pts.len(), "{id:?} range");
                }
                Prim::Text { range, .. } => {
                    assert!(range.end as usize <= sc.text.len(), "{id:?} text range");
                }
                Prim::Rect { rect, .. } => assert!(rect.x0.is_finite() && rect.y1.is_finite()),
            }
        }
    }

    /// Every plot type answers a hover with exact values somewhere.
    #[test]
    fn every_chart_has_hover_values() {
        let mut core = core_with(1800.0);
        for (id, h) in charts() {
            let sc = core.scene(id, W, h, 13.0).clone();
            check_scene(id, &sc, W, h);
            let mut found = false;
            let mut y = 1.0;
            while y < h && !found {
                let mut x = 1.0;
                while x < W && !found {
                    if let Some(tip) = core.hover(id, W, h, x, y).tip {
                        assert!(!tip.is_empty(), "{id:?} empty tip");
                        found = true;
                    }
                    x += 7.0;
                }
                y += 3.0;
            }
            assert!(found, "{id:?} shows no tooltip anywhere");
        }
    }

    /// Scenes stay cheap and bounded with days of data.
    #[test]
    #[ignore = "slow in debug builds; run with --release"]
    fn soak_multi_day_scenes_are_fast_and_bounded() {
        let mut core = core_with(4.0 * 86_400.0);
        for zoom in [None, Some(2.0e3), Some(30.0)] {
            core.reset_view();
            if let Some(f) = zoom {
                core.zoom(4.0 * 86_400.0 / f, W - 100.0, W);
            }
            for (id, h) in charts() {
                let t = Instant::now();
                let sc = core.scene(id, W, h, 13.0);
                let n = sc.pts.len();
                let took = t.elapsed();
                check_scene(id, &sc.clone(), W, h);
                assert!(
                    took < Duration::from_millis(30),
                    "{id:?} took {took:?} ({zoom:?})"
                );
                // M4 keeps at most 4 points per column per series, plus
                // aggregates, bands, ticks and dots.
                assert!(n < 40 * W as usize, "{id:?} has {n} points ({zoom:?})");
            }
            let t = Instant::now();
            let f = core.frame();
            assert!(
                t.elapsed() < Duration::from_millis(10),
                "frame took {:?}",
                t.elapsed()
            );
            assert_eq!(f.headline.len(), 5);
            let t = Instant::now();
            for x in (50..950).step_by(50) {
                let _ = core.hover(ChartId::Latency, W, 148.0, x as f32, 80.0);
                let _ = core.hover(ChartId::Throughput, W, 196.0, x as f32, 100.0);
            }
            assert!(
                t.elapsed() < Duration::from_millis(40),
                "hovers took {:?}",
                t.elapsed()
            );
        }
    }

    #[test]
    fn scenes_are_cached_until_data_or_size_changes() {
        let mut core = core_with(600.0);
        let a = core.scene(ChartId::Box(BoxId::Latency), W, 96.0, 13.0) as *const Scene;
        let b = core.scene(ChartId::Box(BoxId::Latency), W, 96.0, 13.0) as *const Scene;
        assert_eq!(a, b);
        let n1 = core
            .scene(ChartId::Box(BoxId::Latency), W, 96.0, 13.0)
            .pts
            .len();
        let n2 = core
            .scene(ChartId::Box(BoxId::Latency), W / 2.0, 96.0, 13.0)
            .pts
            .len();
        assert!(n1 >= n2);
    }

    #[test]
    fn export_writes_off_thread_and_reports_in_the_status() {
        let mut core = core_with(60.0);
        let path =
            std::env::temp_dir().join(format!("rst-export-test-{}.json", std::process::id()));
        core.export(path.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            let f = core.frame();
            if f.status.text.starts_with("Export") || Instant::now() > deadline {
                break f.status.text;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let written = std::fs::read_to_string(&path);
        let _ = std::fs::remove_file(&path);
        assert!(status.starts_with("Exported to"), "{status}");
        assert!(written.is_ok_and(|j| crate::net::json::Json::parse(&j).is_some()));
    }

    #[test]
    fn settings_apply_to_config() {
        let mut core = Core::new(Arc::new(|_| {}));
        core.set(Setting::Interval, 0);
        assert_eq!(
            core.config().continuous.cycle_interval,
            Duration::from_secs(60)
        );
        core.set(Setting::Interval, 99);
        assert_eq!(
            core.config().continuous.cycle_interval,
            Duration::from_secs(60)
        );
    }
}
