//! The test plan: a one-shot test, or continuous cycles. Throughput is
//! measured in sustained phases: several streams keep the link loaded in
//! one direction for a fixed time.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::sustained::{self, Cut, Plan};
use super::{ConnSlot, RunCtx, latency, meta, packet_loss, retry_delay};
use crate::model::{ConnPurpose, Direction, Event, Load, Phase, RunMode};
use crate::net::http::NetError;

/// Unloaded latency probes before the throughput phases.
const LATENCY_PROBES: u32 = 20;
/// Length of each one-shot throughput phase.
pub const ONE_SHOT_SUSTAIN: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u32 = 3;
/// A one-shot phase cut by HTTP 429 is run again after a wait up to this
/// long; a longer `Retry-After` ends that direction.
const ONE_SHOT_MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
const MAX_CYCLE_BACKOFF: Duration = Duration::from_secs(30 * 60);

pub fn run(ctx: Arc<RunCtx>) {
    match ctx.mode {
        RunMode::OneShot => one_shot(&ctx),
        RunMode::Continuous => continuous(&ctx),
    }
}

fn load_of(d: Direction) -> Load {
    match d {
        Direction::Download => Load::Download,
        Direction::Upload => Load::Upload,
    }
}

/// Connection slots for `n` streams in direction `dir`.
fn slots(dir: Direction, n: u32) -> Vec<ConnSlot> {
    let purpose = match dir {
        Direction::Download => ConnPurpose::Download,
        Direction::Upload => ConnPurpose::Upload,
    };
    (0..n.max(1)).map(|_| ConnSlot::new(purpose)).collect()
}

/// One sustained phase with the link marked loaded, so latency and loss
/// probes sent meanwhile count as loaded.
fn sustain(
    ctx: &RunCtx,
    slots: &mut Vec<ConnSlot>,
    dir: Direction,
    length: Duration,
    on_progress: &mut dyn FnMut(f64),
) -> Option<Cut> {
    let streams = ctx.config().streams.max(1);
    if slots.len() != streams as usize {
        *slots = self::slots(dir, streams);
    }
    ctx.set_load(load_of(dir));
    let (_, cut) = sustained::run(
        ctx,
        slots,
        &Plan {
            dir,
            streams,
            length,
        },
        on_progress,
    );
    ctx.set_load(Load::Idle);
    cut
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
    // Progress in tenths of a second of expected test time.
    let phase_units = (ONE_SHOT_SUSTAIN.as_secs_f64() * 10.0) as u32;
    let total: u32 = 10 + LATENCY_PROBES + 40 + 2 * phase_units;
    let mut done: u32 = 0;
    let progress = |ctx: &RunCtx, done: u32| {
        ctx.emit(Event::Progress(done as f32 / total.max(1) as f32));
    };
    progress(ctx, done);

    if !metadata(ctx, &mut meta_slot) {
        if !ctx.stopping() {
            finish(ctx);
        }
        return;
    }
    done += 10;
    progress(ctx, done);

    let ok = latency_burst(ctx, &mut meta_slot, LATENCY_PROBES, &mut || {
        done += 1;
        progress(ctx, done);
    });
    if !ok {
        return;
    }
    // The metadata connection would idle through the rest of the test.
    meta_slot.reset();

    // Packet loss on the unloaded link.
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
    done += 40;
    progress(ctx, done);

    for dir in [Direction::Download, Direction::Upload] {
        let mut slots = slots(dir, ctx.config().streams);
        let base = done;
        let mut attempt = 0;
        loop {
            if !ctx.wait_running() {
                return;
            }
            let cut = sustain(ctx, &mut slots, dir, ONE_SHOT_SUSTAIN, &mut |f| {
                progress(ctx, base + (f * f64::from(phase_units)) as u32);
            });
            match cut {
                None => break,
                Some(Cut::Interrupted) => {
                    if ctx.stopping() {
                        return;
                    }
                    // Paused: run the phase again from the start.
                    continue;
                }
                Some(Cut::RateLimited(after)) => {
                    attempt += 1;
                    let wait = after.unwrap_or(Duration::from_secs(5 << attempt));
                    if attempt >= MAX_ATTEMPTS || wait > ONE_SHOT_MAX_RETRY_WAIT {
                        break;
                    }
                    ctx.phase(Phase::BackingOff {
                        until: ctx.now() + wait.as_secs_f64(),
                        reason: "HTTP 429".into(),
                    });
                    if !ctx.sleep_until(Instant::now() + wait) {
                        return;
                    }
                }
            }
        }
        done = base + phase_units;
        progress(ctx, done);
    }
    finish(ctx);
}

fn continuous(ctx: &RunCtx) {
    let mut meta_slot = ConnSlot::new(ConnPurpose::Metadata);
    let mut failures: u32 = 0;
    loop {
        if !metadata(ctx, &mut meta_slot) {
            return;
        }
        // The metadata connection would idle until the next cycle.
        meta_slot.reset();
        let cycle_start = Instant::now();
        // The server's own wait, when it asked for one.
        let mut not_before: Option<Instant> = None;
        for dir in [Direction::Download, Direction::Upload] {
            let length = ctx.config().continuous.sustain;
            // Fresh connections each cycle: the next cycle records a new
            // DNS/TCP/TLS setup instead of reusing idle ones.
            let mut slots = slots(dir, ctx.config().streams);
            match sustain(ctx, &mut slots, dir, length, &mut |_| {}) {
                None => {}
                Some(Cut::Interrupted) => {
                    if ctx.stopping() {
                        return;
                    }
                }
                Some(Cut::RateLimited(after)) => {
                    let wait = after.unwrap_or(Duration::from_secs(60));
                    not_before = Some(Instant::now() + wait);
                    break;
                }
            }
        }
        failures = if not_before.is_some() {
            failures + 1
        } else {
            0
        };
        // Wait for the next cycle, re-reading the interval so a changed
        // setting applies to the current wait.
        loop {
            let interval = ctx.config().continuous.cycle_interval;
            let wait = (interval * (1u32 << failures.min(6))).min(MAX_CYCLE_BACKOFF.max(interval));
            let next = (cycle_start + wait).max(not_before.unwrap_or(cycle_start));
            let now = Instant::now();
            if now >= next {
                break;
            }
            let at = ctx.now() + (next - now).as_secs_f64();
            ctx.phase(if failures > 0 {
                Phase::BackingOff {
                    until: at,
                    reason: "HTTP 429".into(),
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
