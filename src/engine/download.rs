use std::time::{Duration, Instant};

use super::sampler::Binner;
use super::{RunCtx, ms};
use crate::model::{Direction, RequestRecord};
use crate::net::cloudflare;
use crate::net::http::{Conn, NetError};
use crate::platform;

pub struct Spec {
    pub size: u64,
    pub index: u32,
    pub count: u32,
    /// Stop reading after this long and keep the partial transfer.
    pub limit: Duration,
}

/// One `GET __down?bytes=size`.
///
/// Throughput follows Cloudflare's definition so results are comparable:
/// `bits / (TTFB - server time + first-to-last-byte time)`, i.e. one
/// network round trip plus the payload transfer.
pub fn run(
    ctx: &RunCtx,
    conn: &mut Conn,
    conn_id: u32,
    new_conn: bool,
    spec: &Spec,
) -> Result<RequestRecord, NetError> {
    let id = ctx.id();
    let mut binner = Binner::new(ctx, Direction::Download, id, None);
    let deadline = Some(Instant::now() + spec.limit);
    let ex = conn.get(
        &cloudflare::down_path(spec.size),
        &|| ctx.interrupted(),
        deadline,
        &mut |t, n| binner.add(t, n),
    )?;
    binner.finish(ex.t_end, ex.body_bytes);
    let server_ms = cloudflare::server_time_ms(ex.head.all("server-timing"));
    let ttfb_ms = ms(ex.t_first_byte - ex.t_start);
    let transfer_ms = ms(ex.t_end.saturating_duration_since(ex.t_first_byte));
    let ping_ms = (ttfb_ms - server_ms.unwrap_or(0.0)).max(0.0);
    let duration_ms = ping_ms + transfer_ms;
    let bps = (duration_ms > 0.0 && ex.body_bytes > 0)
        .then(|| ex.body_bytes as f64 * 8.0 / (duration_ms / 1000.0));
    Ok(RequestRecord {
        id,
        conn: conn_id,
        new_conn,
        dir: Direction::Download,
        size: spec.size,
        bytes: ex.body_bytes,
        index: spec.index,
        count: spec.count,
        t_start: ctx.at(ex.t_start),
        t_end: ctx.at(ex.t_end),
        ttfb_ms,
        transfer_ms,
        server_ms,
        duration_ms,
        bps,
        complete: ex.complete,
        server_tcp: cloudflare::server_tcp(ex.head.all("server-timing")),
        client_tcp: platform::current().tcp_stats(conn.socket()),
    })
}
