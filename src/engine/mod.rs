//! Test orchestration. The engine runs on its own threads, takes `Command`s
//! and reports `Event`s over channels. It knows nothing about the UI.
//!
//! A run has four workers:
//! - `schedule`: the test plan (one-shot suite or continuous cycles), which
//!   runs sustained transfers on a thread per stream,
//! - `latency`: periodic and loaded latency probes on their own connection,
//! - `packet_loss`: UDP STUN probes,
//! - `local`: gateway pings, Wi-Fi and interface sampling.

mod download;
mod latency;
mod local;
mod meta;
mod packet_loss;
mod sampler;
mod schedule;
mod sustained;
mod upload;

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::model::{Command, Config, ConnPurpose, ConnRecord, Event, Load, Phase, RunMode, Secs};
use crate::net::cloudflare;
use crate::net::http::{Conn, NetError};

pub struct Engine {
    cmd: Sender<Command>,
}

impl Engine {
    pub fn spawn(events: Sender<Event>) -> Engine {
        let (cmd, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || supervise(rx, events))
            .expect("spawn engine thread");
        Engine { cmd }
    }

    pub fn send(&self, c: Command) {
        let _ = self.cmd.send(c);
    }
}

fn supervise(rx: Receiver<Command>, events: Sender<Event>) {
    let mut run: Option<Run> = None;
    for cmd in rx {
        match cmd {
            Command::Start(mode, config) => {
                if let Some(r) = run.take() {
                    r.stop();
                }
                run = Some(Run::start(mode, config, events.clone()));
            }
            Command::Stop => {
                if let Some(r) = run.take() {
                    let t = r.ctx.now();
                    let already = r.ctx.plan_done();
                    r.stop();
                    if !already {
                        let _ = events.send(Event::Finished(t));
                    }
                }
            }
            // A finished one-shot run has nothing left to pause.
            Command::Pause => {
                if let Some(r) = run.as_ref().filter(|r| !r.ctx.plan_done()) {
                    r.ctx.set_state(State::Paused);
                }
            }
            Command::Resume => {
                if let Some(r) = run.as_ref().filter(|r| !r.ctx.plan_done()) {
                    r.ctx.set_state(State::Running);
                }
            }
            Command::Reconfigure(cfg) => {
                if let Some(r) = &run {
                    *r.ctx.config.lock().expect("config lock") = cfg;
                    r.ctx.emit(Event::Reconfigured(cfg));
                    let _st = r.ctx.state.lock().expect("state lock");
                    r.ctx.cv.notify_all();
                }
            }
        }
    }
    if let Some(r) = run.take() {
        r.stop();
    }
}

struct Run {
    ctx: Arc<RunCtx>,
    workers: Vec<JoinHandle<()>>,
}

impl Run {
    fn start(mode: RunMode, config: Config, events: Sender<Event>) -> Run {
        let ctx = Arc::new(RunCtx {
            t0: Instant::now(),
            mode,
            config: Mutex::new(config),
            state: Mutex::new(State::Running),
            cv: Condvar::new(),
            load: AtomicU8::new(Load::Idle.as_u8()),
            local_ip: Mutex::new(None),
            plan_done: AtomicBool::new(false),
            loss_pending: AtomicUsize::new(0),
            ids: AtomicU32::new(1),
            events,
        });
        ctx.emit(Event::RunStarted {
            mode,
            config,
            wall: SystemTime::now(),
        });
        let spawn = |name: &str, f: fn(Arc<RunCtx>)| {
            let c = ctx.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || f(c))
                .expect("spawn worker")
        };
        let workers = vec![
            spawn("schedule", schedule::run),
            spawn("latency", latency::run),
            spawn("packet-loss", packet_loss::run),
            spawn("local", local::run),
        ];
        Run { ctx, workers }
    }

    fn stop(self) {
        self.ctx.set_state(State::Stopping);
        for w in self.workers {
            let _ = w.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running,
    Paused,
    Stopping,
}

/// State shared by the workers of one run.
pub(crate) struct RunCtx {
    t0: Instant,
    mode: RunMode,
    config: Mutex<Config>,
    state: Mutex<State>,
    cv: Condvar,
    /// What the schedule is loading the link with right now.
    load: AtomicU8,
    /// Local address of the most recent connection, for interface lookup.
    local_ip: Mutex<Option<IpAddr>>,
    /// Set when a one-shot plan has finished.
    plan_done: AtomicBool,
    /// Probes the background UDP prober has sent but not yet emitted.
    loss_pending: AtomicUsize,
    ids: AtomicU32,
    events: Sender<Event>,
}

impl RunCtx {
    fn now(&self) -> Secs {
        self.at(Instant::now())
    }

    fn at(&self, i: Instant) -> Secs {
        i.saturating_duration_since(self.t0).as_secs_f64()
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn phase(&self, p: Phase) {
        self.emit(Event::Phase(p));
    }

    fn error(&self, message: String) {
        self.emit(Event::Error {
            t: self.now(),
            message,
        });
    }

    fn id(&self) -> u32 {
        self.ids.fetch_add(1, Ordering::Relaxed)
    }

    fn config(&self) -> Config {
        *self.config.lock().expect("config lock")
    }

    fn load(&self) -> Load {
        Load::from_u8(self.load.load(Ordering::Relaxed))
    }

    fn set_load(&self, l: Load) {
        // Under the state lock so a waiter between its load check and its
        // wait can't miss the notification.
        let _st = self.state.lock().expect("state lock");
        self.load.store(l.as_u8(), Ordering::Relaxed);
        self.cv.notify_all();
    }

    /// Wait until `deadline` of running time has passed, sitting out any
    /// pause in between (a server-requested wait isn't cut short by
    /// pausing and resuming). Returns false once the run is stopping.
    fn sleep_until(&self, deadline: Instant) -> bool {
        let mut st = self.state.lock().expect("state lock");
        loop {
            match *st {
                State::Stopping => return false,
                State::Paused => st = self.cv.wait(st).expect("state lock"),
                State::Running => {
                    let now = Instant::now();
                    if now >= deadline {
                        return true;
                    }
                    st = self
                        .cv
                        .wait_timeout(st, deadline - now)
                        .expect("state lock")
                        .0;
                }
            }
        }
    }

    /// Wait (up to the loss timeout) until the background UDP prober has
    /// emitted every outstanding probe, so loss results don't straddle a
    /// phase boundary or arrive after the run finished.
    fn wait_loss_drained(&self) {
        let limit = Instant::now() + packet_loss::LOSS_TIMEOUT + Duration::from_millis(200);
        while self.loss_pending.load(Ordering::Relaxed) > 0 && Instant::now() < limit {
            if !self.sleep(Duration::from_millis(20)) && !self.wait_running() {
                return;
            }
        }
    }

    fn plan_done(&self) -> bool {
        self.plan_done.load(Ordering::Relaxed)
    }

    fn set_local_ip(&self, ip: IpAddr) {
        *self.local_ip.lock().expect("ip lock") = Some(ip);
    }

    fn local_ip(&self) -> Option<IpAddr> {
        *self.local_ip.lock().expect("ip lock")
    }

    fn state(&self) -> State {
        *self.state.lock().expect("state lock")
    }

    fn set_state(&self, s: State) {
        let mut st = self.state.lock().expect("state lock");
        if *st == State::Stopping || *st == s {
            return;
        }
        *st = s;
        drop(st);
        match s {
            State::Paused => self.emit(Event::Paused(self.now())),
            State::Running => self.emit(Event::Resumed(self.now())),
            State::Stopping => {}
        }
        self.cv.notify_all();
    }

    /// True when in-flight work should be abandoned (pause or stop).
    fn interrupted(&self) -> bool {
        self.state() != State::Running
    }

    fn stopping(&self) -> bool {
        self.state() == State::Stopping
    }

    /// Block while paused. Returns false once the run is stopping.
    fn wait_running(&self) -> bool {
        let mut st = self.state.lock().expect("state lock");
        while *st == State::Paused {
            st = self.cv.wait(st).expect("state lock");
        }
        *st == State::Running
    }

    /// Sleep for `d` unless paused or stopped first. Returns true when the
    /// full time elapsed while running.
    fn sleep(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        let mut st = self.state.lock().expect("state lock");
        loop {
            if *st != State::Running {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            st = self
                .cv
                .wait_timeout(st, deadline - now)
                .expect("state lock")
                .0;
        }
    }

    /// Sleep until `d` passes or the load changes. Returns false on
    /// pause/stop.
    fn sleep_or_load_change(&self, d: Duration) -> bool {
        let start_load = self.load();
        let deadline = Instant::now() + d;
        let mut st = self.state.lock().expect("state lock");
        loop {
            if *st != State::Running {
                return false;
            }
            let now = Instant::now();
            if now >= deadline || self.load() != start_load {
                return true;
            }
            st = self
                .cv
                .wait_timeout(st, deadline - now)
                .expect("state lock")
                .0;
        }
    }
}

/// A lazily opened keep-alive connection that reopens itself after errors.
pub(crate) struct ConnSlot {
    purpose: ConnPurpose,
    conn: Option<(Conn, u32)>,
}

impl ConnSlot {
    pub(crate) fn new(purpose: ConnPurpose) -> ConnSlot {
        ConnSlot {
            purpose,
            conn: None,
        }
    }

    /// The connection, its id, and whether it was opened by this call.
    pub(crate) fn get(&mut self, ctx: &RunCtx) -> Result<(&mut Conn, u32, bool), NetError> {
        if self.conn.as_ref().is_some_and(|(c, _)| !c.usable()) {
            self.conn = None;
        }
        let mut new = false;
        if self.conn.is_none() {
            let family = ctx.config().family;
            let t = ctx.now();
            let c = Conn::open(cloudflare::HOST, cloudflare::PORT, family, &|| {
                ctx.interrupted()
            })?;
            let id = ctx.id();
            ctx.set_local_ip(c.local.ip());
            ctx.emit(Event::Conn(ConnRecord {
                id,
                t,
                purpose: self.purpose,
                remote: c.remote,
                local: c.local,
                dns_ms: ms(c.dns),
                connect_ms: ms(c.connect),
                tls_ms: ms(c.tls_handshake),
                tls_version: c.tls_version(),
                cipher: c.cipher(),
                alpn: c.alpn(),
            }));
            self.conn = Some((c, id));
            new = true;
        }
        let (c, id) = self.conn.as_mut().expect("opened above");
        Ok((c, *id, new))
    }

    pub(crate) fn reset(&mut self) {
        self.conn = None;
    }
}

pub(crate) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// How long to wait before retrying after `e`, or `None` if the error
/// should not be retried (pause/stop).
fn retry_delay(e: &NetError, attempt: u32) -> Option<Duration> {
    match e {
        NetError::Aborted => None,
        NetError::Http {
            status: 429,
            retry_after,
        } => Some(
            retry_after
                .unwrap_or(Duration::from_secs(5 << attempt.min(4)))
                .min(Duration::from_secs(3600)),
        ),
        _ => Some(Duration::from_secs(1 << attempt.min(5))),
    }
}

/// Format bytes as a payload label: `100 kB`, `25 MB`.
pub fn size_label(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 1_000_000 {
        let mb = b / 1e6;
        if (mb - mb.round()).abs() < 0.05 {
            format!("{:.0} MB", mb)
        } else {
            format!("{:.1} MB", mb)
        }
    } else if bytes >= 1000 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(size_label(100_000), "100 kB");
        assert_eq!(size_label(1_000_000), "1 MB");
        assert_eq!(size_label(25_000_000), "25 MB");
        assert_eq!(size_label(3_400_000), "3.4 MB");
        assert_eq!(size_label(0), "0 B");
    }

    #[test]
    fn retry_backs_off() {
        assert_eq!(retry_delay(&NetError::Aborted, 0), None);
        assert_eq!(
            retry_delay(&NetError::Closed, 0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retry_delay(&NetError::Closed, 2),
            Some(Duration::from_secs(4))
        );
        let e429 = NetError::Http {
            status: 429,
            retry_after: Some(Duration::from_secs(9)),
        };
        assert_eq!(retry_delay(&e429, 0), Some(Duration::from_secs(9)));
    }
}
