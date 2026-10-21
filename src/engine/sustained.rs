//! Sustained transfers: several connections kept busy with back-to-back
//! requests in one direction for a fixed time, so the link stays loaded
//! for the whole phase instead of one request at a time. The streams add
//! their bytes to one shared total, which is binned every 50 ms.

use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::download::{self, Sink, Spec};
use super::sampler::Binner;
use super::{ConnSlot, RunCtx, retry_delay, upload};
use crate::model::{Direction, Event, Phase, SustainedRecord};
use crate::net::cloudflare;
use crate::net::http::NetError;
use crate::platform;

/// How often the shared byte total is read into bins.
const POLL: Duration = Duration::from_millis(5);
/// How often the phase reports its progress.
const PROGRESS: Duration = Duration::from_millis(100);
/// The first part of a phase, while TCP ramps up, isn't counted in its
/// throughput: this long, or a fifth of the phase if that is shorter.
const RAMP: Duration = Duration::from_secs(2);
/// Upload bodies per request: large enough that a stream rarely starts a
/// new request, which would idle it for a round trip.
const UPLOAD_BYTES: u64 = 50_000_000;
/// A stream that fails waits at least this long before its next request.
const STREAM_RETRY: Duration = Duration::from_millis(500);

/// Upload progress the binning thread reads from a stream's socket every
/// poll: bytes the stream has written minus bytes still in the socket's
/// send buffer (not yet sent, or sent and not yet acknowledged). A stream
/// blocked in a write reports progress only when the write returns, which
/// under a deep send queue lumps many milliseconds into one bin.
#[derive(Default)]
struct Watch {
    /// Bytes written to the stream's sockets so far.
    written: AtomicU64,
    /// A handle on the stream's current socket, and the bytes left
    /// unacknowledged on sockets the stream has dropped.
    sock: Mutex<(Option<TcpStream>, u64)>,
}

impl Watch {
    fn queued(sock: &TcpStream) -> Option<u64> {
        platform::current().tcp_stats(sock)?.send_queue_bytes
    }

    fn sock(&self) -> std::sync::MutexGuard<'_, (Option<TcpStream>, u64)> {
        self.sock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bytes acknowledged so far.
    fn read(&self) -> u64 {
        let (sock, lost) = &*self.sock();
        let queued = sock.as_ref().and_then(Self::queued).unwrap_or(0);
        self.written
            .load(Ordering::Relaxed)
            .saturating_sub(lost + queued)
    }

    /// Starts reading `sock`; false when this platform doesn't report a
    /// socket's send queue.
    fn attach(&self, sock: &TcpStream) -> bool {
        self.detach();
        if Self::queued(sock).is_none() {
            return false;
        }
        let mut g = self.sock();
        g.0 = sock.try_clone().ok();
        g.0.is_some()
    }

    /// Stops reading the current socket; what it never got acknowledged is
    /// kept out of the count. The handle is dropped so the connection can
    /// close.
    fn detach(&self) {
        let mut g = self.sock();
        if let Some(s) = g.0.take() {
            g.1 += Self::queued(&s).unwrap_or(0);
        }
    }
}

pub struct Plan {
    pub dir: Direction,
    pub streams: u32,
    pub length: Duration,
}

/// Why a sustained transfer ended early.
pub enum Cut {
    /// Paused or stopped.
    Interrupted,
    /// The server asked to wait (HTTP 429 with this `Retry-After`).
    RateLimited(Option<Duration>),
}

/// Runs `plan` on `slots` (one per stream, kept open across phases).
/// Emits each request, the summed bins and the record; returns the record,
/// and why it ended early if it did.
pub fn run(
    ctx: &RunCtx,
    slots: &mut [ConnSlot],
    plan: &Plan,
    on_progress: &mut dyn FnMut(f64),
) -> (Option<SustainedRecord>, Option<Cut>) {
    let id = ctx.id();
    let total = AtomicU64::new(0);
    let requests = AtomicU32::new(0);
    let stop = AtomicBool::new(false);
    let cut: Mutex<Option<Cut>> = Mutex::new(None);
    let start = Instant::now();
    let deadline = start + plan.length;
    let ramp = RAMP.min(plan.length / 5);
    ctx.phase(Phase::Sustained {
        dir: plan.dir,
        streams: plan.streams,
        started: ctx.at(start),
        length: plan.length.as_secs_f64(),
    });
    let sink = Sink {
        total: &total,
        sustained: id,
        written: None,
        stop: &stop,
    };
    let mut binner = Binner::new(ctx, plan.dir, id, Some(start));
    let mut steady_base: Option<(Instant, u64)> = None;
    let streams = (plan.streams as usize).min(slots.len()).max(1);
    let watches: Vec<Watch> = (0..streams).map(|_| Watch::default()).collect();
    let watched = || -> u64 { watches.iter().map(Watch::read).sum() };
    std::thread::scope(|sc| {
        for (i, slot) in slots.iter_mut().take(streams).enumerate() {
            let (stop, cut, requests) = (&stop, &cut, &requests);
            let watch = &watches[i];
            std::thread::Builder::new()
                .name(format!("stream-{}", i + 1))
                .spawn_scoped(sc, move || {
                    stream(
                        ctx,
                        slot,
                        plan,
                        i as u32 + 1,
                        deadline,
                        sink,
                        watch,
                        stop,
                        cut,
                        requests,
                    )
                })
                .expect("spawn stream");
        }
        // Bin the shared total until the phase ends.
        let mut reported = start;
        loop {
            let now = Instant::now();
            if now.saturating_duration_since(reported) >= PROGRESS {
                reported = now;
                on_progress(
                    now.saturating_duration_since(start).as_secs_f64()
                        / plan.length.as_secs_f64().max(1e-3),
                );
            }
            let bytes = total.load(Ordering::Relaxed) + watched();
            binner.add(now, bytes);
            if steady_base.is_none() && now >= start + ramp {
                steady_base = Some((now, bytes));
            }
            if now >= deadline || stop.load(Ordering::Relaxed) {
                break;
            }
            if !ctx.sleep(POLL.min(deadline - now)) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    });
    let end = Instant::now();
    let bytes = total.load(Ordering::Relaxed) + watched();
    binner.finish(end, bytes);
    let cut = cut.into_inner().unwrap_or_else(|e| e.into_inner());
    let cut = cut.or_else(|| ctx.interrupted().then_some(Cut::Interrupted));
    let (t_steady, steady_bytes) = steady_base.unwrap_or((start, 0));
    let steady_secs = end.saturating_duration_since(t_steady).as_secs_f64();
    let steady = bytes.saturating_sub(steady_bytes);
    if bytes == 0 {
        return (None, cut);
    }
    // A transfer cut short ran on fewer connections or for less time than
    // planned; its bytes are kept, but it has no throughput value.
    let bps = (cut.is_none() && steady_secs > 0.0 && steady > 0)
        .then(|| steady as f64 * 8.0 / steady_secs);
    let rec = SustainedRecord {
        id,
        dir: plan.dir,
        streams: streams as u32,
        t_start: ctx.at(start),
        t_end: ctx.at(end),
        bytes,
        t_steady: ctx.at(t_steady),
        steady_bytes: steady,
        bps,
        requests: requests.load(Ordering::Relaxed),
        complete: cut.is_none(),
    };
    ctx.emit(Event::Sustained(rec.clone()));
    (Some(rec), cut)
}

/// One stream: back-to-back requests until `deadline`.
#[allow(clippy::too_many_arguments)] // One stream's whole context; used once.
fn stream(
    ctx: &RunCtx,
    slot: &mut ConnSlot,
    plan: &Plan,
    index: u32,
    deadline: Instant,
    sink: Sink<'_>,
    watch: &Watch,
    stop: &AtomicBool,
    cut: &Mutex<Option<Cut>>,
    requests: &AtomicU32,
) {
    // A watched stream's bytes are counted from its socket, not again
    // through the shared total.
    let ignored = AtomicU64::new(0);
    // The connection the watch is reading.
    let mut watching: Option<u32> = None;
    let mut attempt = 0;
    loop {
        let now = Instant::now();
        if now >= deadline || stop.load(Ordering::Relaxed) || ctx.interrupted() {
            return;
        }
        let spec = Spec {
            size: match plan.dir {
                Direction::Download => cloudflare::MAX_DOWNLOAD_BYTES,
                Direction::Upload => UPLOAD_BYTES,
            },
            index,
            count: plan.streams,
            limit: deadline - now,
        };
        let r = slot.get(ctx).and_then(|(c, id, new)| match plan.dir {
            Direction::Download => download::run(ctx, c, id, new, &spec, sink),
            Direction::Upload => {
                if watching != Some(id) {
                    watching = watch.attach(c.socket()).then_some(id);
                }
                let sink = match watching {
                    Some(_) => Sink {
                        total: &ignored,
                        written: Some(&watch.written),
                        ..sink
                    },
                    None => sink,
                };
                upload::run(ctx, c, id, new, &spec, sink)
            }
        });
        // A connection about to be dropped is read one last time.
        let mut reset = |slot: &mut ConnSlot| {
            if watching.take().is_some() {
                watch.detach();
            }
            slot.reset();
        };
        match r {
            Ok(rec) => {
                attempt = 0;
                requests.fetch_add(1, Ordering::Relaxed);
                // A request cut at the deadline leaves its connection
                // mid-body; the slot reopens it next phase.
                if !rec.complete {
                    reset(slot);
                }
                ctx.emit(Event::Request(rec));
            }
            Err(NetError::Aborted) => {
                reset(slot);
                return;
            }
            Err(e) => {
                reset(slot);
                let suffix = match &e {
                    NetError::Http {
                        retry_after: Some(r),
                        ..
                    } => format!(" (retry after {} s)", r.as_secs()),
                    _ => String::new(),
                };
                ctx.error(format!(
                    "{} stream {index} failed: {e}{suffix}",
                    plan.dir.label()
                ));
                if let NetError::Http {
                    status: 429,
                    retry_after,
                } = e
                {
                    let mut c = cut.lock().unwrap_or_else(|e| e.into_inner());
                    c.get_or_insert(Cut::RateLimited(retry_after));
                    stop.store(true, Ordering::Relaxed);
                    return;
                }
                attempt += 1;
                let wait = retry_delay(&e, attempt - 1)
                    .unwrap_or_default()
                    .max(STREAM_RETRY);
                let until = (Instant::now() + wait).min(deadline);
                if !ctx.sleep(until.saturating_duration_since(Instant::now())) {
                    return;
                }
            }
        }
    }
}
