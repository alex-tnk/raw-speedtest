//! DNS answers, per-family reachability and Cloudflare's view of the client.

use std::net::SocketAddr;

use super::{ConnSlot, RunCtx, ms, packet_loss};
use crate::model::{EdgeInfo, Event, HostInfo, Sourced};
use crate::net::cloudflare;
use crate::net::http::{self, NetError};

/// Resolve the test host and time a TCP connect over each address family.
pub fn host(ctx: &RunCtx) -> Result<HostInfo, NetError> {
    let r = http::resolve(cloudflare::HOST, cloudflare::PORT)?;
    let v4: Vec<SocketAddr> = r
        .addrs
        .iter()
        .copied()
        .filter(SocketAddr::is_ipv4)
        .collect();
    let v6: Vec<SocketAddr> = r.addrs.iter().copied().filter(|a| a.is_ipv6()).collect();
    let try_connect = |addrs: &[SocketAddr], fam: &str| -> Option<Result<f64, String>> {
        if ctx.interrupted() {
            return None;
        }
        Some(match addrs.first() {
            None => Err(format!("no {fam} address")),
            Some(a) => http::tcp_connect(*a)
                .map(|(_, d)| ms(d))
                .map_err(|e| e.to_string()),
        })
    };
    let info = HostInfo {
        host: cloudflare::HOST.into(),
        v4_connect_ms: try_connect(&v4, "IPv4"),
        v6_connect_ms: try_connect(&v6, "IPv6"),
        v4: v4.iter().map(SocketAddr::ip).collect(),
        v6: v6.iter().map(SocketAddr::ip).collect(),
        dns_ms: Some(ms(r.elapsed)),
        used: http::pick(&r.addrs, ctx.config().family).ok(),
    };
    ctx.emit(Event::Host(info.clone()));
    Ok(info)
}

/// Read edge metadata over `slot`: `__down` headers, the trace endpoint,
/// `/meta` (empty without a Referer, which the app doesn't send) and the
/// UDP-mapped address from STUN.
pub fn edge(ctx: &RunCtx, slot: &mut ConnSlot) -> Result<EdgeInfo, NetError> {
    let abort = || ctx.interrupted();
    let (conn, _, _) = slot.get(ctx)?;
    let (ex, _) = conn.get_text(&cloudflare::down_path(0), &abort)?;
    let mut edge = cloudflare::edge_from_headers(|k| ex.head.header(k));
    match conn.get_text(cloudflare::TRACE_PATH, &abort) {
        Ok((_, body)) => cloudflare::merge_trace(&mut edge, &cloudflare::parse_trace(&body)),
        Err(NetError::Aborted) => return Err(NetError::Aborted),
        Err(e) => ctx.error(format!("Trace request failed: {e}")),
    }
    let (conn, _, _) = slot.get(ctx)?;
    match conn.get_text(cloudflare::META_PATH, &abort) {
        Ok((_, body)) => cloudflare::merge_meta(&mut edge, &body),
        Err(NetError::Aborted) => return Err(NetError::Aborted),
        // Optional source: its absence is shown as unavailable fields.
        Err(_) => {}
    }
    if let Some(m) = packet_loss::mapped_address(ctx, ctx.config().family) {
        edge.udp_mapped = Some(Sourced::new(m.to_string(), cloudflare::SRC_STUN));
    }
    ctx.emit(Event::Edge(edge.clone()));
    Ok(edge)
}
