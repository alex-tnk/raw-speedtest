use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ConnSlot, RunCtx, ms};
use crate::model::{ConnPurpose, Event, LatencySample, Load, RunMode};
use crate::net::cloudflare;
use crate::net::http::{Conn, NetError};

/// Spacing of loaded-latency probes (Cloudflare's `loadedLatencyThrottle`).
pub const LOADED_INTERVAL: Duration = Duration::from_millis(400);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// One `GET __down?bytes=0` on a warm connection. Latency is TTFB minus
/// the server processing time the edge reports in `Server-Timing`, so it
/// reflects the network path rather than the worker.
pub fn probe(
    ctx: &RunCtx,
    conn: &mut Conn,
    conn_id: u32,
    load: Load,
    index: u32,
    count: u32,
) -> Result<LatencySample, NetError> {
    let ex = conn.get(
        &cloudflare::down_path(0),
        &|| ctx.interrupted(),
        None,
        &mut |_, _| {},
    )?;
    let ttfb_ms = ms(ex.t_first_byte - ex.t_start);
    let server_ms = cloudflare::server_time_ms(ex.head.all("server-timing"));
    let server_rtt_ms = cloudflare::server_tcp(ex.head.all("server-timing"))
        .and_then(|s| s.rtt_us)
        .map(|us| us as f64 / 1000.0);
    Ok(LatencySample {
        t: ctx.at(ex.t_start),
        ms: (ttfb_ms - server_ms.unwrap_or(0.0)).max(0.0),
        ttfb_ms,
        server_ms,
        load,
        server_rtt_ms,
        conn: conn_id,
        index,
        count,
    })
}

/// Background prober: during transfers it measures loaded latency every
/// 400 ms; in continuous mode it also probes the idle link at the
/// configured interval.
pub fn run(ctx: Arc<RunCtx>) {
    let mut slot = ConnSlot::new(match ctx.mode {
        RunMode::OneShot => ConnPurpose::LoadedLatency,
        RunMode::Continuous => ConnPurpose::Latency,
    });
    let mut failures = 0u32;
    loop {
        if !ctx.wait_running() || ctx.plan_done() {
            return;
        }
        let load = ctx.load();
        let interval = match (load, ctx.mode) {
            (Load::Idle, RunMode::OneShot) => None,
            (Load::Idle, RunMode::Continuous) => Some(ctx.config().continuous.latency_interval),
            _ => Some(LOADED_INTERVAL),
        };
        let Some(interval) = interval else {
            ctx.sleep_or_load_change(Duration::from_millis(250));
            continue;
        };
        let started = Instant::now();
        let r = slot
            .get(&ctx)
            .and_then(|(c, id, _)| probe(&ctx, c, id, load, 0, 0));
        let wait = match r {
            Ok(s) => {
                failures = 0;
                ctx.emit(Event::Latency(s));
                interval
            }
            Err(NetError::Aborted) => {
                slot.reset();
                continue;
            }
            Err(e) => {
                slot.reset();
                failures += 1;
                ctx.error(format!("Latency probe failed: {e}"));
                let backoff = super::retry_delay(&e, failures - 1).unwrap_or(interval);
                if let NetError::Http {
                    status: 429,
                    retry_after: Some(r),
                } = e
                {
                    // The server said how long to stay away: sit it out in
                    // full, through pauses and load changes.
                    if !ctx.sleep_until(started + r.max(interval)) {
                        return;
                    }
                    continue;
                }
                backoff.max(interval).min(MAX_BACKOFF)
            }
        };
        let elapsed = started.elapsed();
        ctx.sleep_or_load_change(wait.saturating_sub(elapsed));
    }
}
