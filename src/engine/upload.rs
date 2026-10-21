use std::time::Instant;

use super::download::{Sink, Spec};
use super::{RunCtx, ms};
use crate::model::{Direction, RequestRecord};
use crate::net::cloudflare;
use crate::net::http::{Conn, NetError};
use crate::platform;

/// One `POST __up` with `size` bytes.
///
/// Throughput follows Cloudflare's definition: `bits / (request start to
/// first response byte)`; the edge answers only after it has the whole
/// body. Progress counts bytes the peer has acknowledged (kernel TCP
/// counters), not bytes handed to the socket buffer, so the buffer
/// filling doesn't show as a burst.
pub fn run(
    ctx: &RunCtx,
    conn: &mut Conn,
    conn_id: u32,
    new_conn: bool,
    spec: &Spec,
    sink: Sink<'_>,
) -> Result<RequestRecord, NetError> {
    let id = ctx.id();
    let plat = platform::current();
    let acked_base = plat.tcp_stats(conn.socket()).and_then(|s| s.bytes_acked);
    let start = Instant::now();
    let mut prev = 0;
    let mut written = 0u64;
    let mut acked: Option<u64> = None;
    let deadline = Some(start + spec.limit);
    let r = conn.post(
        cloudflare::UP_PATH,
        spec.size,
        &|| ctx.interrupted(),
        deadline,
        &mut |_, w, sock| {
            written = w;
            let stats = plat.tcp_stats(sock);
            let a = stats.and_then(|s| match (acked_base, s.bytes_acked, s.send_queue_bytes) {
                (Some(base), Some(now), _) => Some(now.saturating_sub(base)),
                (_, _, Some(queued)) => Some(w.saturating_sub(queued)),
                _ => None,
            });
            if let Some(a) = a {
                let a = acked.map_or(a, |prev| prev.max(a));
                acked = Some(a);
                sink.progress(&mut prev, a);
            }
        },
    )?;
    let client_tcp = plat.tcp_stats(conn.socket());
    let rec = match r {
        Ok(ex) => {
            if acked.is_some() {
                sink.progress(&mut prev, written);
            }
            let ttfb_ms = ms(ex.t_first_byte - ex.t_start);
            let duration_ms = ttfb_ms;
            RequestRecord {
                id,
                conn: conn_id,
                new_conn,
                sustained: Some(sink.sustained),
                dir: Direction::Upload,
                size: spec.size,
                bytes: spec.size,
                index: spec.index,
                count: spec.count,
                t_start: ctx.at(ex.t_start),
                t_end: ctx.at(ex.t_first_byte),
                ttfb_ms,
                transfer_ms: ms(ex.t_sent - ex.t_start),
                server_ms: cloudflare::server_time_ms(ex.head.all("server-timing")),
                duration_ms,
                bps: (duration_ms > 0.0).then(|| spec.size as f64 * 8.0 / (duration_ms / 1000.0)),
                complete: true,
                server_tcp: cloudflare::server_tcp(ex.head.all("server-timing")),
                client_tcp,
            }
        }
        Err(t_cut) => {
            // Time limit hit mid-body: rate from acknowledged bytes only.
            if let Some(a) = acked {
                sink.progress(&mut prev, a);
            }
            let dur = ms(t_cut - start);
            RequestRecord {
                id,
                conn: conn_id,
                new_conn,
                sustained: Some(sink.sustained),
                dir: Direction::Upload,
                size: spec.size,
                bytes: acked.unwrap_or(0),
                index: spec.index,
                count: spec.count,
                t_start: ctx.at(start),
                t_end: ctx.at(t_cut),
                ttfb_ms: f64::NAN,
                transfer_ms: dur,
                server_ms: None,
                duration_ms: dur,
                bps: acked
                    .filter(|_| dur > 0.0)
                    .map(|a| a as f64 * 8.0 / (dur / 1000.0)),
                complete: false,
                server_tcp: None,
                client_tcp,
            }
        }
    };
    Ok(rec)
}
