//! The test plan: Cloudflare's one-shot sequence, or continuous cycles.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::download::{self, Spec};
use super::{ConnSlot, RunCtx, latency, meta, packet_loss, retry_delay, size_label, upload};
use crate::model::{ConnPurpose, Direction, Event, Load, Payload, Phase, RequestRecord, RunMode};
use crate::net::cloudflare::MAX_DOWNLOAD_BYTES;
use crate::net::http::NetError;

#[derive(Clone, Copy, Debug)]
enum Step {
    Latency(u32),
    Transfer(Direction, u64, u32),
    PacketLoss,
}

/// Cloudflare's default measurement sequence (speedtest 1.14), with the
/// sizes the endpoints accept today: `__down` refuses 100 MB and up, so the
/// top download tier is 99,999,999 bytes.
const ONE_SHOT: &[Step] = &[
    Step::Latency(20),
    Step::Transfer(Direction::Download, 100_000, 10),
    Step::Transfer(Direction::Download, 1_000_000, 8),
    Step::Transfer(Direction::Upload, 100_000, 8),
    Step::PacketLoss,
    Step::Transfer(Direction::Upload, 1_000_000, 6),
    Step::Transfer(Direction::Download, 10_000_000, 6),
    Step::Transfer(Direction::Upload, 10_000_000, 4),
    Step::Transfer(Direction::Download, 25_000_000, 4),
    Step::Transfer(Direction::Upload, 25_000_000, 4),
    Step::Transfer(Direction::Download, 50_000_000, 3),
    Step::Transfer(Direction::Upload, 50_000_000, 3),
    Step::Transfer(Direction::Download, MAX_DOWNLOAD_BYTES, 3),
];

/// Once every request of a set took longer than this, larger sizes in that
/// direction are skipped (Cloudflare's `bandwidthFinishRequestDuration`).
const FINISH_DURATION_MS: f64 = 1000.0;
/// Loaded-latency probes count only during requests at least this long
/// (Cloudflare's `loadedRequestMinDuration`).
const LOADED_MIN_MS: f64 = 250.0;
const ONE_SHOT_LIMIT: Duration = Duration::from_secs(15);
const CONTINUOUS_LIMIT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u32 = 3;
const ONE_SHOT_MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
const MAX_CYCLE_BACKOFF: Duration = Duration::from_secs(30 * 60);
const AUTO_TARGET: Duration = Duration::from_secs(1);
const AUTO_MIN: u64 = 100_000;
const AUTO_MAX: u64 = 25_000_000;

pub fn run(ctx: Arc<RunCtx>) {
    match ctx.mode {
        RunMode::OneShot => one_shot(&ctx),
        RunMode::Continuous => continuous(&ctx),
    }
}

/// Outcome of a set of same-size requests.
enum SetEnd {
    Done { min_duration_ms: f64 },
    Failed,
    Stopped,
}

/// Last per-request throughput per direction, used to predict durations.
#[derive(Default)]
struct Rates {
    down: Option<f64>,
    up: Option<f64>,
}

impl Rates {
    fn get(&self, d: Direction) -> Option<f64> {
        match d {
            Direction::Download => self.down,
            Direction::Upload => self.up,
        }
    }
    fn set(&mut self, d: Direction, bps: f64) {
        match d {
            Direction::Download => self.down = Some(bps),
            Direction::Upload => self.up = Some(bps),
        }
    }
}

fn load_of(d: Direction) -> Load {
    match d {
        Direction::Download => Load::Download,
        Direction::Upload => Load::Upload,
    }
}

/// Run one request with retries. `None` means stopped or given up.
fn request(
    ctx: &RunCtx,
    slot: &mut ConnSlot,
    dir: Direction,
    spec: &Spec,
) -> Result<RequestRecord, SetEnd> {
    let mut attempt = 0;
    loop {
        if !ctx.wait_running() {
            return Err(SetEnd::Stopped);
        }
        ctx.phase(Phase::Transfer {
            dir,
            size: spec.size,
            index: spec.index,
            count: spec.count,
        });
        let r = slot.get(ctx).and_then(|(c, id, new)| match dir {
            Direction::Download => download::run(ctx, c, id, new, spec),
            Direction::Upload => upload::run(ctx, c, id, new, spec),
        });
        match r {
            Ok(rec) => return Ok(rec),
            Err(NetError::Aborted) => slot.reset(),
            Err(e) => {
                slot.reset();
                attempt += 1;
                let wait = retry_delay(&e, attempt - 1).unwrap_or_default();
                // A one-shot test doesn't sit out a long Retry-After: that
                // direction ends and the plan moves on.
                let give_up = attempt >= MAX_ATTEMPTS
                    || (ctx.mode == RunMode::OneShot && wait > ONE_SHOT_MAX_RETRY_WAIT);
                let suffix = match &e {
                    NetError::Http {
                        retry_after: Some(r),
                        ..
                    } => format!(" (retry after {} s)", r.as_secs()),
                    _ => String::new(),
                };
                ctx.error(format!(
                    "{} {} failed: {e}{suffix}",
                    dir.label(),
                    size_label(spec.size)
                ));
                if give_up {
                    return Err(SetEnd::Failed);
                }
                ctx.phase(Phase::BackingOff {
                    until: ctx.now() + wait.as_secs_f64(),
                    reason: e.to_string(),
                });
                // Nothing loads the link while waiting.
                let load = ctx.load();
                ctx.set_load(Load::Idle);
                let slept = ctx.sleep_until(Instant::now() + wait);
                ctx.set_load(load);
                if !slept {
                    return Err(SetEnd::Stopped);
                }
            }
        }
    }
}

/// A set of same-direction requests.
struct SetSpec<'a> {
    dir: Direction,
    /// Payload of the next request given the rates measured so far.
    size_for: &'a dyn Fn(&Rates) -> u64,
    count: u32,
    limit: Duration,
}

/// Run `set.count` requests. Marks the link as loaded (for the latency and
/// loss probers) only while requests last long enough to load it, as
/// Cloudflare does.
fn transfer_set(
    ctx: &RunCtx,
    slot: &mut ConnSlot,
    rates: &mut Rates,
    set: &SetSpec<'_>,
    on_done: &mut dyn FnMut(),
) -> SetEnd {
    let SetSpec {
        dir,
        size_for,
        count,
        limit,
    } = *set;
    let first = size_for(rates);
    let predicted_ms = rates.get(dir).map(|bps| first as f64 * 8.0 / bps * 1000.0);
    let mut loaded = predicted_ms.is_some_and(|p| p >= LOADED_MIN_MS);
    ctx.set_load(if loaded { load_of(dir) } else { Load::Idle });
    let mut min_duration_ms = f64::INFINITY;
    for index in 1..=count {
        let size = size_for(rates);
        let spec = Spec {
            size,
            index,
            count,
            limit,
        };
        let rec = match request(ctx, slot, dir, &spec) {
            Ok(r) => r,
            Err(end) => {
                ctx.set_load(Load::Idle);
                return end;
            }
        };
        min_duration_ms = min_duration_ms.min(rec.duration_ms);
        if let Some(bps) = rec.bps {
            rates.set(dir, bps);
        }
        if !loaded && rec.duration_ms >= LOADED_MIN_MS {
            loaded = true;
            ctx.set_load(load_of(dir));
        }
        ctx.emit(Event::Request(rec));
        on_done();
    }
    ctx.set_load(Load::Idle);
    SetEnd::Done { min_duration_ms }
}

/// Unloaded latency burst on the metadata connection.
fn latency_burst(ctx: &RunCtx, slot: &mut ConnSlot, n: u32, on_done: &mut dyn FnMut()) -> bool {
    let mut i = 1;
    let mut attempt = 0;
    while i <= n {
        if !ctx.wait_running() {
            return false;
        }
        ctx.phase(Phase::Latency {
            done: i - 1,
            total: n,
        });
        let r = slot
            .get(ctx)
            .and_then(|(c, id, _)| latency::probe(ctx, c, id, Load::Idle, i, n));
        match r {
            Ok(s) => {
                ctx.emit(Event::Latency(s));
                i += 1;
                attempt = 0;
                on_done();
            }
            Err(NetError::Aborted) => slot.reset(),
            Err(e) => {
                slot.reset();
                ctx.error(format!("Latency probe failed: {e}"));
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return true;
                }
                if !ctx
                    .sleep_until(Instant::now() + retry_delay(&e, attempt - 1).unwrap_or_default())
                {
                    return false;
                }
            }
        }
    }
    ctx.phase(Phase::Latency { done: n, total: n });
    true
}

/// Host and edge metadata, retried until it works or the run stops.
fn metadata(ctx: &RunCtx, slot: &mut ConnSlot) -> bool {
    let mut attempt = 0;
    loop {
        if !ctx.wait_running() {
            return false;
        }
        ctx.phase(Phase::Connecting);
        let r = meta::host(ctx).and_then(|_| meta::edge(ctx, slot));
        match r {
            Ok(_) => return true,
            Err(NetError::Aborted) => slot.reset(),
            Err(e) => {
                slot.reset();
                ctx.error(format!("Connection failed: {e}"));
                attempt += 1;
                let wait = retry_delay(&e, attempt - 1).unwrap_or_default();
                if ctx.mode == RunMode::OneShot && attempt >= MAX_ATTEMPTS {
                    return false;
                }
                ctx.phase(Phase::BackingOff {
                    until: ctx.now() + wait.as_secs_f64(),
                    reason: e.to_string(),
                });
                if !ctx.sleep_until(Instant::now() + wait.min(MAX_CYCLE_BACKOFF)) {
                    return false;
                }
            }
        }
    }
}

fn finish(ctx: &RunCtx) {
    ctx.set_load(Load::Idle);
    // Loss probes sent during the last loaded set still count.
    ctx.wait_loss_drained();
    ctx.plan_done.store(true, Ordering::Relaxed);
    ctx.emit(Event::Progress(1.0));
    ctx.emit(Event::Finished(ctx.now()));
    ctx.cv.notify_all();
}

fn one_shot(ctx: &RunCtx) {
    let mut meta_slot = ConnSlot::new(ConnPurpose::Metadata);
    let mut down_slot = ConnSlot::new(ConnPurpose::Download);
    let mut up_slot = ConnSlot::new(ConnPurpose::Upload);
    let mut rates = Rates::default();

    let weight = |s: &Step| match *s {
        Step::Latency(n) => n,
        Step::Transfer(_, _, n) => n,
        Step::PacketLoss => 10,
    };
    let mut total: u32 = 1 + ONE_SHOT.iter().map(weight).sum::<u32>();
    let mut done: u32 = 0;
    let progress = |ctx: &RunCtx, done: u32, total: u32| {
        ctx.emit(Event::Progress(done as f32 / total.max(1) as f32));
    };
    progress(ctx, done, total);

    if !metadata(ctx, &mut meta_slot) {
        if !ctx.stopping() {
            finish(ctx);
        }
        return;
    }
    done += 1;
    progress(ctx, done, total);

    let mut finished_dir = [false, false];
    let idx = |d: Direction| match d {
        Direction::Download => 0,
        Direction::Upload => 1,
    };
    for step in ONE_SHOT {
        match *step {
            Step::Latency(n) => {
                let ok = latency_burst(ctx, &mut meta_slot, n, &mut || {
                    done += 1;
                    progress(ctx, done, total);
                });
                if !ok {
                    return;
                }
            }
            Step::PacketLoss => {
                // Loaded probes still in flight resolve first, so the
                // probe timeline stays in time order.
                ctx.wait_loss_drained();
                let mut sent = 0;
                loop {
                    match packet_loss::phase(ctx, &mut sent) {
                        Ok(true) => break,
                        Ok(false) => {
                            if !ctx.wait_running() {
                                return;
                            }
                        }
                        Err(e) => {
                            ctx.error(format!("Packet loss test failed: {e}"));
                            break;
                        }
                    }
                }
                done += 10;
                progress(ctx, done, total);
            }
            Step::Transfer(dir, size, count) => {
                if finished_dir[idx(dir)] {
                    total -= count;
                    progress(ctx, done, total);
                    continue;
                }
                let slot = match dir {
                    Direction::Download => &mut down_slot,
                    Direction::Upload => &mut up_slot,
                };
                let end = transfer_set(
                    ctx,
                    slot,
                    &mut rates,
                    &SetSpec {
                        dir,
                        size_for: &|_| size,
                        count,
                        limit: ONE_SHOT_LIMIT,
                    },
                    &mut || {
                        done += 1;
                        progress(ctx, done, total);
                    },
                );
                match end {
                    SetEnd::Done { min_duration_ms } => {
                        if min_duration_ms > FINISH_DURATION_MS {
                            finished_dir[idx(dir)] = true;
                        }
                    }
                    SetEnd::Failed => finished_dir[idx(dir)] = true,
                    SetEnd::Stopped => return,
                }
            }
        }
    }
    finish(ctx);
}

/// Payload for the next continuous request in `dir`.
fn auto_size(rates: &Rates, dir: Direction) -> u64 {
    let Some(bps) = rates.get(dir) else {
        return 1_000_000;
    };
    let bytes = bps / 8.0 * AUTO_TARGET.as_secs_f64();
    round_2sig(bytes.clamp(AUTO_MIN as f64, AUTO_MAX as f64))
}

/// Round to two significant digits so payload labels stay readable.
fn round_2sig(x: f64) -> u64 {
    if x <= 0.0 {
        return 0;
    }
    let mag = 10f64.powf(x.log10().floor() - 1.0);
    ((x / mag).round() * mag) as u64
}

fn continuous(ctx: &RunCtx) {
    let mut meta_slot = ConnSlot::new(ConnPurpose::Metadata);
    let mut down_slot = ConnSlot::new(ConnPurpose::Download);
    let mut up_slot = ConnSlot::new(ConnPurpose::Upload);
    let mut rates = Rates::default();
    let mut failures: u32 = 0;
    loop {
        if !metadata(ctx, &mut meta_slot) {
            return;
        }
        // The metadata connection would idle until the next cycle.
        meta_slot.reset();
        let cycle_start = Instant::now();
        let mut failed = false;
        for dir in [Direction::Download, Direction::Upload] {
            let cfg = ctx.config().continuous;
            let size_for = |r: &Rates| {
                let s = match cfg.payload {
                    Payload::Auto => auto_size(r, dir),
                    Payload::Bytes(b) => b,
                };
                match dir {
                    Direction::Download => s.min(MAX_DOWNLOAD_BYTES),
                    Direction::Upload => s,
                }
            };
            let slot = match dir {
                Direction::Download => &mut down_slot,
                Direction::Upload => &mut up_slot,
            };
            match transfer_set(
                ctx,
                slot,
                &mut rates,
                &SetSpec {
                    dir,
                    size_for: &size_for,
                    count: cfg.requests.max(1),
                    limit: CONTINUOUS_LIMIT,
                },
                &mut || {},
            ) {
                SetEnd::Done { .. } => {}
                SetEnd::Failed => failed = true,
                SetEnd::Stopped => return,
            }
            // Idle connections are closed between cycles; the next cycle
            // records a fresh DNS/TCP/TLS setup.
            match dir {
                Direction::Download => down_slot.reset(),
                Direction::Upload => up_slot.reset(),
            }
        }
        failures = if failed { failures + 1 } else { 0 };
        // Wait for the next cycle, re-reading the interval so a changed
        // setting applies to the current wait.
        loop {
            let interval = ctx.config().continuous.cycle_interval;
            let wait = (interval * (1u32 << failures.min(6))).min(MAX_CYCLE_BACKOFF.max(interval));
            let next = cycle_start + wait;
            let now = Instant::now();
            if now >= next {
                break;
            }
            let at = ctx.now() + (next - now).as_secs_f64();
            ctx.phase(if failures > 0 {
                Phase::BackingOff {
                    until: at,
                    reason: "throughput cycle failed".into(),
                }
            } else {
                Phase::Waiting { next_cycle_at: at }
            });
            if !ctx.sleep((next - now).min(Duration::from_secs(1))) && !ctx.wait_running() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_payloads() {
        assert_eq!(round_2sig(3_456_789.0), 3_500_000);
        assert_eq!(round_2sig(100_000.0), 100_000);
        assert_eq!(round_2sig(99_999.0), 100_000);
        let r = Rates {
            down: Some(80e6),
            up: None,
        };
        assert_eq!(auto_size(&r, Direction::Download), 10_000_000);
        assert_eq!(auto_size(&r, Direction::Upload), 1_000_000);
        let fast = Rates {
            down: Some(10e9),
            up: Some(1e3),
        };
        assert_eq!(auto_size(&fast, Direction::Download), AUTO_MAX);
        assert_eq!(auto_size(&fast, Direction::Upload), AUTO_MIN);
    }

    #[test]
    fn plan_sizes_are_accepted() {
        for s in ONE_SHOT {
            if let Step::Transfer(Direction::Download, b, _) = s {
                assert!(*b <= MAX_DOWNLOAD_BYTES);
            }
        }
    }
}
