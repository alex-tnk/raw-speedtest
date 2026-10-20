//! App state and commands: owns the engine, collects its events into the
//! session on a dedicated thread, and exposes the run controls.

pub mod derived;
pub mod export;
pub mod session;

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::engine::Engine;
use crate::model::{Command, Config, Event, RunMode};
use session::Session;

/// Asks the UI to redraw within the given delay. Called from the collector
/// thread after a batch of events; it must only schedule a frame, never
/// draw.
pub type Waker = Arc<dyn Fn(Duration) + Send + Sync>;

/// Redraw delay while a transfer streams throughput samples.
const LIVE_FRAME: Duration = Duration::from_millis(33);
/// Redraw delay for background probe results: several per second arrive
/// in continuous mode and are shown together, about once per default
/// latency probe interval.
const PROBE_FRAME: Duration = Duration::from_secs(1);

pub struct Controller {
    engine: Engine,
    session: Arc<Mutex<Session>>,
    /// Settings for the next run (and the current one, for continuous).
    pub config: Config,
    pub mode: RunMode,
}

impl Controller {
    pub fn new(wake: Waker) -> Controller {
        let (tx, rx) = std::sync::mpsc::channel();
        let engine = Engine::spawn(tx);
        let session = Arc::new(Mutex::new(Session::new()));
        let s = session.clone();
        std::thread::Builder::new()
            .name("collector".into())
            .spawn(move || collect(rx, s, wake))
            .expect("spawn collector");
        Controller {
            engine,
            session,
            config: Config::default(),
            mode: RunMode::OneShot,
        }
    }

    pub fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The session, for work done off the UI thread (export).
    pub fn shared(&self) -> Arc<Mutex<Session>> {
        self.session.clone()
    }

    pub fn start(&self) {
        self.engine.send(Command::Start(self.mode, self.config));
    }

    pub fn stop(&self) {
        self.engine.send(Command::Stop);
    }

    pub fn pause(&self) {
        self.engine.send(Command::Pause);
    }

    pub fn resume(&self) {
        self.engine.send(Command::Resume);
    }

    /// Clear all data. An active run restarts from scratch.
    pub fn reset(&self) {
        let (active, mode) = {
            let s = self.session();
            (s.active(), s.mode)
        };
        match (active, mode) {
            (true, Some(m)) => self.engine.send(Command::Start(m, self.config)),
            _ => self.session().clear(),
        }
    }

    pub fn apply_config(&self) {
        self.engine.send(Command::Reconfigure(self.config));
    }
}

fn collect(rx: Receiver<Event>, session: Arc<Mutex<Session>>, wake: Waker) {
    let probe_only = |e: &Event| {
        matches!(
            e,
            Event::Latency(_)
                | Event::Loss(_)
                | Event::Gateway(_)
                | Event::WifiSample(_)
                | Event::Wifi(_)
        )
    };
    while let Ok(ev) = rx.recv() {
        let mut background = probe_only(&ev);
        {
            let mut s = session.lock().unwrap_or_else(|e| e.into_inner());
            s.apply(ev);
            // Apply whatever else is queued under the same lock.
            while let Ok(ev) = rx.try_recv() {
                background &= probe_only(&ev);
                s.apply(ev);
            }
        }
        wake(if background { PROBE_FRAME } else { LIVE_FRAME });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::derived::Cache;
    use super::session::Session;
    use crate::engine::Engine;
    use crate::model::{Command, Config, Direction, Event, Load, RunMode};

    /// Full one-shot run against the live endpoints:
    /// `cargo test live_one_shot -- --ignored --nocapture`
    #[test]
    #[ignore = "needs network access"]
    fn live_one_shot() {
        let (tx, rx) = std::sync::mpsc::channel();
        let engine = Engine::spawn(tx);
        engine.send(Command::Start(RunMode::OneShot, Config::default()));
        let mut s = Session::new();
        loop {
            let ev = rx
                .recv_timeout(Duration::from_secs(300))
                .expect("engine stalled");
            match &ev {
                Event::Error { message, .. } => eprintln!("error: {message}"),
                Event::Conn(c) => eprintln!(
                    "conn {} {:?} {} dns {:.1} tcp {:.1} tls {:.1} {}",
                    c.id, c.purpose, c.remote, c.dns_ms, c.connect_ms, c.tls_ms, c.tls_version
                ),
                Event::Request(r) => eprintln!(
                    "{:?} {} {}/{} ttfb {:.1} xfer {:.1} srv {:?} dur {:.1} {:.2} Mbps tcp {:?}",
                    r.dir,
                    r.size,
                    r.index,
                    r.count,
                    r.ttfb_ms,
                    r.transfer_ms,
                    r.server_ms,
                    r.duration_ms,
                    r.bps.unwrap_or(0.0) / 1e6,
                    r.server_tcp
                        .map(|t| (t.sent_packets, t.lost_packets, t.retrans_packets)),
                ),
                Event::Interface(i) => eprintln!("iface {i:?}"),
                Event::Wifi(w) => eprintln!("wifi {w:?}"),
                Event::Edge(e) => eprintln!("edge {e:?}"),
                Event::Host(h) => eprintln!("host {h:?}"),
                _ => {}
            }
            let done = matches!(ev, Event::Finished(_));
            s.apply(ev);
            if done {
                break;
            }
        }
        let mut c = Cache::new();
        c.update(&s);
        let d = &c.derived;
        eprintln!(
            "down {:?} Mbps, up {:?} Mbps, latency {:?} ms, jitter {:?} ms, loss {:?}",
            d.download.value.map(|v| v / 1e6),
            d.upload.value.map(|v| v / 1e6),
            d.latency.value,
            d.jitter.value,
            d.loss.value
        );
        for l in Load::ALL {
            eprintln!(
                "{:?}: latency {:?} gateway {:?} udp {:?} loss {:?}",
                l,
                d.lat_by_load[l.as_u8() as usize]
                    .summary
                    .map(|x| (x.n, x.median)),
                d.gw_by_load[l.as_u8() as usize]
                    .summary
                    .map(|x| (x.n, x.median)),
                d.udp_rtt_by_load[l.as_u8() as usize]
                    .summary
                    .map(|x| (x.n, x.median)),
                s.loss_counts[l.as_u8() as usize],
            );
        }
        eprintln!("retrans {:?}", d.retrans);
        eprintln!(
            "throughput bins {} wifi samples {} errors {}",
            s.throughput[0].raw.len() + s.throughput[1].raw.len(),
            s.wifi_samples.raw.len(),
            s.errors.len()
        );
        assert!(s.requests_in(Direction::Download).count() > 0);
        assert!(d.download.value.is_some());
        assert!(d.upload.value.is_some());
        assert!(d.latency.value.is_some());
        assert!(d.loss.value.is_some());
    }
}
