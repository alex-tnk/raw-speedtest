//! UDP packet loss and RTT against Cloudflare's public STUN server.
//!
//! Cloudflare's web test measures loss over WebRTC through a TURN relay
//! whose credentials are only issued to its own page. Natively, a STUN
//! Binding request is the closest public equivalent: a small UDP datagram
//! to the same anycast edge that is answered exactly once. A probe that
//! gets no answer within 3 s (Cloudflare's `responsesWaitTime`) is lost;
//! loss here is round-trip loss, either direction.

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::RunCtx;
use crate::model::{Event, IpFamily, Load, LossProbe, Phase, ProbeOutcome, RunMode};
use crate::net::{cloudflare, http, stun};

pub const LOSS_TIMEOUT: Duration = Duration::from_secs(3);
/// Probe spacing while a one-shot transfer loads the link.
const LOADED_SPACING: Duration = Duration::from_millis(50);
/// Dedicated one-shot phase: 1000 probes, 4 ms apart.
pub const PHASE_PROBES: u32 = 1000;
const PHASE_SPACING: Duration = Duration::from_millis(4);
/// How often the background prober checks that its socket still matches
/// the current route.
const ROUTE_CHECK: Duration = Duration::from_secs(2);
/// How many resolved sequence numbers are remembered for late/duplicate
/// detection.
const HISTORY: usize = 20_000;

struct Inflight {
    seq: u32,
    sent: Instant,
    load: Load,
}

/// A probe slot: waiting for its answer, or answered but held back so
/// results leave in send order (a lost probe resolves only after the
/// timeout, and later answers wait behind it).
enum Slot {
    Pending(Inflight),
    Answered(Inflight, f64),
}

pub struct Prober {
    sock: UdpSocket,
    peer: SocketAddr,
    /// A send failed: the socket no longer has a usable route.
    broken: bool,
    nonce: u64,
    next_seq: u32,
    /// Probes by sequence number, oldest first, until emitted.
    inflight: VecDeque<Slot>,
    base_seq: u32,
    /// Recently resolved probes: (seq, sent, load, lost?).
    history: VecDeque<(u32, Instant, Load, bool)>,
    pub mapped: Option<SocketAddr>,
}

impl Prober {
    pub fn open(family: IpFamily) -> Result<Prober, String> {
        let r = http::resolve(cloudflare::STUN_HOST, cloudflare::STUN_PORT)
            .map_err(|e| e.to_string())?;
        let addr = http::pick(&r.addrs, family).map_err(|e| e.to_string())?;
        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse().expect("valid")
        } else {
            "[::]:0".parse().expect("valid")
        };
        let sock = UdpSocket::bind(bind).map_err(|e| e.to_string())?;
        sock.connect(addr).map_err(|e| e.to_string())?;
        let nonce = {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            t ^ (u64::from(std::process::id()) << 32) ^ 0x5DEE_CE66_D1CE_4E5B
        };
        Ok(Prober {
            sock,
            peer: addr,
            broken: false,
            nonce,
            next_seq: 0,
            inflight: VecDeque::new(),
            base_seq: 0,
            history: VecDeque::new(),
            mapped: None,
        })
    }

    pub fn pending(&self) -> usize {
        self.inflight.len()
    }

    /// True when the socket should be replaced: a send failed, or the
    /// system would now send to the server from a different local address
    /// (the network changed; a connected UDP socket keeps its old source
    /// address and every probe would be dropped).
    fn stale(&self) -> bool {
        if self.broken {
            return true;
        }
        let Ok(current) = self.sock.local_addr() else {
            return true;
        };
        let unspecified: SocketAddr = if self.peer.is_ipv4() {
            "0.0.0.0:0".parse().expect("valid")
        } else {
            "[::]:0".parse().expect("valid")
        };
        // Connecting a UDP socket only looks up the route; nothing is sent.
        match UdpSocket::bind(unspecified).and_then(|s| s.connect(self.peer).map(|_| s)) {
            Ok(s) => s.local_addr().map_or(true, |a| a.ip() != current.ip()),
            Err(_) => true,
        }
    }

    pub fn send(&mut self, load: Load) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let msg = stun::request(seq, self.nonce);
        let sent = Instant::now();
        // A send the kernel refuses never reached the network: it is not a
        // probe, and the socket needs replacing (e.g. the route went away).
        if self.sock.send(&msg).is_err() {
            self.broken = true;
            return;
        }
        if self.inflight.is_empty() {
            self.base_seq = seq;
        }
        self.inflight
            .push_back(Slot::Pending(Inflight { seq, sent, load }));
    }

    /// Receive answers until `until`, then expire probes older than the
    /// loss timeout. Returns early when the run is interrupted.
    pub fn poll(&mut self, ctx: &RunCtx, until: Instant) {
        let mut buf = [0u8; 512];
        loop {
            let now = Instant::now();
            self.expire(ctx, now);
            if now >= until || ctx.interrupted() {
                return;
            }
            let wait = (until - now).clamp(Duration::from_millis(1), Duration::from_millis(100));
            let _ = self.sock.set_read_timeout(Some(wait));
            let Ok(n) = self.sock.recv(&mut buf) else {
                continue;
            };
            let at = Instant::now();
            let Some(resp) = stun::parse(&buf[..n], self.nonce) else {
                continue;
            };
            if resp.mapped.is_some() {
                self.mapped = resp.mapped;
            }
            self.resolve(ctx, resp.seq, at);
        }
    }

    fn resolve(&mut self, ctx: &RunCtx, seq: u32, at: Instant) {
        // An answer past the timeout is late even if no poll expired its
        // probe yet.
        self.expire(ctx, at);
        let off = seq.wrapping_sub(self.base_seq) as usize;
        if let Some(slot) = self.inflight.get_mut(off) {
            if let Slot::Pending(p) = slot {
                let rtt = (at - p.sent).as_secs_f64() * 1000.0;
                let p = Inflight {
                    seq: p.seq,
                    sent: p.sent,
                    load: p.load,
                };
                *slot = Slot::Answered(p, rtt);
                self.expire(ctx, at);
                return;
            }
            // Answered but not yet emitted: a duplicate.
            if let Slot::Answered(p, _) = slot {
                ctx.emit(Event::Loss(LossProbe {
                    t: ctx.at(p.sent),
                    seq,
                    outcome: ProbeOutcome::Duplicate,
                    rtt_ms: Some((at - p.sent).as_secs_f64() * 1000.0),
                    load: p.load,
                }));
            }
            return;
        }
        if let Some(h) = self.history.iter_mut().rev().find(|h| h.0 == seq) {
            let (s, sent, load, lost) = *h;
            // Only the first answer to a lost probe is late; repeats are
            // duplicates.
            h.3 = false;
            ctx.emit(Event::Loss(LossProbe {
                t: ctx.at(sent),
                seq: s,
                outcome: if lost {
                    ProbeOutcome::Late
                } else {
                    ProbeOutcome::Duplicate
                },
                rtt_ms: Some((at - sent).as_secs_f64() * 1000.0),
                load,
            }));
        }
    }

    fn remember(&mut self, seq: u32, sent: Instant, load: Load, lost: bool) {
        if self.history.len() >= HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((seq, sent, load, lost));
    }

    /// Emit, in send order, every probe at the front that is answered or
    /// timed out.
    fn expire(&mut self, ctx: &RunCtx, now: Instant) {
        while let Some(front) = self.inflight.front() {
            let (p, outcome, rtt_ms) = match front {
                Slot::Answered(p, rtt) => (p, ProbeOutcome::Received, Some(*rtt)),
                Slot::Pending(p) if now.saturating_duration_since(p.sent) >= LOSS_TIMEOUT => {
                    (p, ProbeOutcome::Lost, None)
                }
                Slot::Pending(_) => return,
            };
            ctx.emit(Event::Loss(LossProbe {
                t: ctx.at(p.sent),
                seq: p.seq,
                outcome,
                rtt_ms,
                load: p.load,
            }));
            let (seq, sent, load) = (p.seq, p.sent, p.load);
            self.remember(seq, sent, load, outcome == ProbeOutcome::Lost);
            self.inflight.pop_front();
            self.base_seq = self.base_seq.wrapping_add(1);
        }
    }

    /// Emit answered probes; drop unanswered ones without judging them
    /// (run paused or stopped mid-wait).
    pub fn forget_inflight(&mut self, ctx: &RunCtx) {
        while let Some(Slot::Answered(..)) = self.inflight.front() {
            self.expire(ctx, Instant::now());
        }
        self.inflight.clear();
    }
}

/// One Binding round trip to learn the UDP-mapped public address. Not
/// counted as a loss probe.
pub fn mapped_address(ctx: &RunCtx, family: IpFamily) -> Option<SocketAddr> {
    let p = Prober::open(family).ok()?;
    let msg = stun::request(u32::MAX, p.nonce);
    let mut buf = [0u8; 512];
    for _ in 0..3 {
        if ctx.interrupted() {
            return None;
        }
        p.sock.send(&msg).ok()?;
        let _ = p.sock.set_read_timeout(Some(Duration::from_millis(500)));
        if let Ok(n) = p.sock.recv(&mut buf)
            && let Some(r) = stun::parse(&buf[..n], p.nonce)
        {
            return r.mapped;
        }
    }
    None
}

/// The dedicated one-shot phase. Returns false if the run was interrupted.
pub fn phase(ctx: &RunCtx, sent_so_far: &mut u32) -> Result<bool, String> {
    let mut p = Prober::open(ctx.config().family)?;
    let start = Instant::now();
    let base = *sent_so_far;
    while *sent_so_far < PHASE_PROBES {
        if ctx.interrupted() {
            p.forget_inflight(ctx);
            return Ok(false);
        }
        p.send(Load::Idle);
        *sent_so_far += 1;
        if (*sent_so_far).is_multiple_of(50) {
            ctx.phase(Phase::PacketLoss {
                sent: *sent_so_far,
                total: PHASE_PROBES,
            });
        }
        let next = start + PHASE_SPACING * (*sent_so_far - base);
        p.poll(ctx, next);
    }
    let last = Instant::now();
    while p.pending() > 0 && last.elapsed() < LOSS_TIMEOUT + Duration::from_millis(100) {
        if ctx.interrupted() {
            p.forget_inflight(ctx);
            return Ok(false);
        }
        p.poll(ctx, Instant::now() + Duration::from_millis(50));
    }
    Ok(true)
}

/// Background prober: continuous mode at the configured rate; one-shot
/// only while a transfer loads the link (packet loss under load).
pub fn run(ctx: Arc<RunCtx>) {
    let mut prober: Option<Prober> = None;
    let mut next_send = Instant::now();
    let mut next_route_check = Instant::now() + ROUTE_CHECK;
    loop {
        ctx.loss_pending.store(
            prober.as_ref().map_or(0, Prober::pending),
            Ordering::Relaxed,
        );
        if ctx.stopping() || ctx.plan_done() {
            return;
        }
        if ctx.interrupted() {
            // Paused: let outstanding probes resolve, then wait.
            if let Some(p) = prober.as_mut() {
                let deadline = Instant::now() + LOSS_TIMEOUT;
                while p.pending() > 0 && Instant::now() < deadline && !ctx.stopping() {
                    std::thread::sleep(Duration::from_millis(20));
                    p.poll_paused(&ctx);
                }
                p.forget_inflight(&ctx);
            }
            if !ctx.wait_running() {
                return;
            }
            next_send = Instant::now();
        }
        let load = ctx.load();
        let spacing = match (ctx.mode, load) {
            (RunMode::Continuous, _) => {
                let hz = ctx.config().continuous.loss_rate_hz;
                (hz > 0.0).then(|| Duration::from_secs_f64(1.0 / hz))
            }
            (RunMode::OneShot, Load::Idle) => None,
            (RunMode::OneShot, _) => Some(LOADED_SPACING),
        };
        let Some(spacing) = spacing else {
            if let Some(p) = prober.as_mut() {
                p.poll(&ctx, Instant::now() + Duration::from_millis(100));
            } else {
                ctx.sleep_or_load_change(Duration::from_millis(250));
            }
            continue;
        };
        if prober.is_none() {
            match Prober::open(ctx.config().family) {
                Ok(p) => prober = Some(p),
                Err(e) => {
                    ctx.error(format!("Packet loss probe failed: {e}"));
                    ctx.sleep(Duration::from_secs(10));
                    continue;
                }
            }
        }
        let p = prober.as_mut().expect("opened above");
        let now = Instant::now();
        if now >= next_route_check {
            next_route_check = now + ROUTE_CHECK;
            if p.stale() {
                // Probes in flight across a network change are dropped
                // without judging them; a fresh socket follows the new route.
                p.forget_inflight(&ctx);
                prober = None;
                continue;
            }
        }
        if now >= next_send {
            p.send(load);
            next_send += spacing;
            if next_send < now {
                // Fell behind (e.g. after a pause): don't burst to catch up.
                next_send = now + spacing;
            }
        }
        p.poll(
            &ctx,
            next_send.min(Instant::now() + Duration::from_millis(100)),
        );
    }
}

impl Prober {
    /// Receive without the interrupted check, used while paused.
    fn poll_paused(&mut self, ctx: &RunCtx) {
        let mut buf = [0u8; 512];
        let _ = self.sock.set_read_timeout(Some(Duration::from_millis(20)));
        while let Ok(n) = self.sock.recv(&mut buf) {
            let at = Instant::now();
            if let Some(resp) = stun::parse(&buf[..n], self.nonce) {
                self.resolve(ctx, resp.seq, at);
            }
        }
        self.expire(ctx, Instant::now());
    }
}
