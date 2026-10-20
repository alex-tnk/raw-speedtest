//! Statistics derived from the session for display. Recomputed only when
//! the session changes, and at most a few times per second for big runs.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::session::{LossCounts, RunState, Session, dir_idx};
use crate::model::{Direction, Load, ProbeOutcome, RunMode};
use crate::stats::{LogHistogram, Summary, jitter, summarize, summarize_iter};

/// Exact order statistics up to this many samples; beyond it, the all-run
/// histogram (within 0.5 %).
const EXACT_MAX: usize = 50_000;
/// Cloudflare ignores requests shorter than this for throughput.
const MIN_REQUEST_MS: f64 = 10.0;
const HEADLINE_WINDOW: f64 = 60.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub summary: Option<Summary>,
    /// Percentiles come from the log histogram, not raw samples.
    pub approx: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Headline {
    pub value: Option<f64>,
    pub summary: Option<Summary>,
    /// What the value is computed over, for the tooltip.
    pub basis: String,
}

#[derive(Clone, Debug)]
pub struct SizeGroup {
    pub size: u64,
    pub summary: Option<Summary>,
    pub incomplete: usize,
}

/// TCP segments reported lost or retransmitted on throughput connections.
#[derive(Clone, Copy, Debug, Default)]
pub struct Retrans {
    pub packets: u64,
    pub retransmitted: u64,
    pub lost: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Window {
    pub label: &'static str,
    pub latency: Option<Summary>,
    pub jitter: Option<f64>,
    pub loss: LossCounts,
    pub gateway: Option<Summary>,
}

#[derive(Clone, Debug, Default)]
pub struct Derived {
    pub version: u64,
    pub download: Headline,
    pub upload: Headline,
    pub latency: Headline,
    pub jitter: Headline,
    pub loss: Headline,
    pub loss_basis: LossCounts,
    pub sizes: [Vec<SizeGroup>; 2],
    /// Median time to first byte per direction, over every request.
    pub ttfb_median: [Option<f64>; 2],
    pub lat_by_load: [Stat; 3],
    pub jitter_by_load: [Option<f64>; 3],
    pub gw_by_load: [Stat; 3],
    pub udp_rtt_by_load: [Stat; 3],
    pub server_rtt: Stat,
    /// Download: edge-reported; upload: this machine's kernel.
    pub retrans: [Option<Retrans>; 2],
    pub windows: Vec<Window>,
}

pub struct Cache {
    pub derived: Derived,
    computed: Option<Instant>,
    scratch: Vec<f64>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Cache {
        Cache {
            derived: Derived::default(),
            computed: None,
            scratch: Vec::new(),
        }
    }

    /// Refresh if the session changed. Large sessions refresh at most every
    /// 250 ms so a long continuous run stays cheap.
    pub fn update(&mut self, s: &Session) {
        if self.derived.version == s.version && self.computed.is_some() {
            return;
        }
        let big = s.latency.raw.len() + s.loss.raw.len() > 20_000;
        if big
            && s.state == RunState::Running
            && self
                .computed
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.derived = compute(s, &mut self.scratch);
        self.computed = Some(Instant::now());
    }
}

fn from_hist(h: &LogHistogram) -> Option<Summary> {
    if h.is_empty() {
        return None;
    }
    Some(Summary {
        n: h.len() as usize,
        min: h.min()?,
        p25: h.percentile(0.25)?,
        median: h.percentile(0.5)?,
        p75: h.percentile(0.75)?,
        p90: h.percentile(0.9)?,
        p99: h.percentile(0.99)?,
        max: h.max()?,
        mean: h.mean()?,
    })
}

fn compute(s: &Session, scratch: &mut Vec<f64>) -> Derived {
    let mut d = Derived {
        version: s.version,
        ..Derived::default()
    };
    let continuous = s.mode == Some(RunMode::Continuous);
    let now = s.now().unwrap_or(0.0);

    // Throughput per direction and size.
    for dir in [Direction::Download, Direction::Upload] {
        let mut groups: BTreeMap<u64, (Vec<f64>, usize)> = BTreeMap::new();
        for r in s.requests_in(dir) {
            let g = groups.entry(r.size).or_default();
            if !r.complete {
                g.1 += 1;
            }
            if let Some(b) = r.bps {
                g.0.push(b);
            }
        }
        d.sizes[dir_idx(dir)] = groups
            .into_iter()
            .map(|(size, (mut v, incomplete))| SizeGroup {
                size,
                summary: summarize(&mut v),
                incomplete,
            })
            .collect();

        d.ttfb_median[dir_idx(dir)] = summarize_iter(
            scratch,
            s.requests_in(dir)
                .map(|r| r.ttfb_ms)
                .filter(|v| v.is_finite()),
        )
        .map(|t| t.median);

        let usable =
            |r: &&crate::model::RequestRecord| r.duration_ms >= MIN_REQUEST_MS && r.bps.is_some();
        let (summary, basis) = if continuous {
            let n = s.config.continuous.requests.max(1) as usize;
            let recent: Vec<f64> = s
                .requests_in(dir)
                .rev()
                .filter(usable)
                .take(n)
                .filter_map(|r| r.bps)
                .collect();
            scratch.clear();
            scratch.extend(recent);
            (
                summarize(scratch),
                format!("90th percentile of the last {n} {} requests", dir.lower()),
            )
        } else {
            (
                summarize_iter(
                    scratch,
                    s.requests_in(dir).filter(usable).filter_map(|r| r.bps),
                ),
                format!(
                    "90th percentile of all {} requests lasting at least {MIN_REQUEST_MS:.0} ms",
                    dir.lower()
                ),
            )
        };
        let h = Headline {
            value: summary.map(|x| x.p90),
            summary,
            basis,
        };
        match dir {
            Direction::Download => d.download = h,
            Direction::Upload => d.upload = h,
        }
    }

    // Latency per load.
    for load in Load::ALL {
        let i = load.as_u8() as usize;
        let hist = &s.latency_hist[i];
        d.lat_by_load[i] = if s.latency.raw_complete() && (hist.len() as usize) <= EXACT_MAX {
            Stat {
                summary: summarize_iter(
                    scratch,
                    s.latency
                        .raw
                        .iter()
                        .filter(|x| x.load == load)
                        .map(|x| x.ms),
                ),
                approx: false,
            }
        } else {
            Stat {
                summary: from_hist(hist),
                approx: true,
            }
        };
        d.jitter_by_load[i] = s.latency_jitter[i].value();
        let ghist = &s.gateway_hist[i];
        d.gw_by_load[i] = if s.gateway.raw_complete() && (ghist.len() as usize) <= EXACT_MAX {
            Stat {
                summary: summarize_iter(
                    scratch,
                    s.gateway
                        .raw
                        .iter()
                        .filter(|x| x.load == load)
                        .filter_map(|x| x.ms),
                ),
                approx: false,
            }
        } else {
            Stat {
                summary: from_hist(ghist),
                approx: true,
            }
        };
        d.udp_rtt_by_load[i] = Stat {
            summary: summarize_iter(
                scratch,
                s.loss
                    .raw
                    .iter()
                    .filter(|x| x.load == load && x.outcome == ProbeOutcome::Received)
                    .filter_map(|x| x.rtt_ms),
            ),
            approx: !s.loss.raw_complete(),
        };
    }
    d.server_rtt = Stat {
        summary: summarize_iter(
            scratch,
            s.latency
                .raw
                .iter()
                .filter(|x| x.load == Load::Idle)
                .filter_map(|x| x.server_rtt_ms),
        ),
        approx: !s.latency.raw_complete(),
    };

    // Headline latency, jitter and loss.
    let idle_window = |from: f64| {
        let (a, b) = s.latency.raw_range(from, f64::INFINITY);
        s.latency.raw.range(a, b).filter(|x| x.load == Load::Idle)
    };
    if continuous {
        let from = now - HEADLINE_WINDOW;
        let lat = summarize_iter(scratch, idle_window(from).map(|x| x.ms));
        d.latency = Headline {
            value: lat.map(|x| x.median),
            summary: lat,
            basis: "Median of unloaded probes in the last 60 s".into(),
        };
        let deltas = deltas(idle_window(from).map(|x| x.ms));
        d.jitter = Headline {
            value: jitter(idle_window(from).map(|x| x.ms)),
            summary: summarize(&mut deltas.clone()),
            basis: "Mean absolute difference between consecutive unloaded probes in the last 60 s"
                .into(),
        };
        let counts = loss_counts(s, from);
        d.loss_basis = counts;
        d.loss = Headline {
            value: counts.rate(),
            summary: None,
            basis: "UDP probes without an answer within 3 s, last 60 s".into(),
        };
    } else {
        let burst = || {
            s.latency
                .raw
                .iter()
                .filter(|x| x.load == Load::Idle && x.index > 0)
        };
        let lat = summarize_iter(scratch, burst().map(|x| x.ms));
        d.latency = Headline {
            value: lat.map(|x| x.median),
            summary: lat,
            basis: "Median of the unloaded latency probes".into(),
        };
        let deltas = deltas(burst().map(|x| x.ms));
        d.jitter = Headline {
            value: jitter(burst().map(|x| x.ms)),
            summary: summarize(&mut deltas.clone()),
            basis: "Mean absolute difference between consecutive unloaded probes".into(),
        };
        let counts = s.loss_counts[0];
        d.loss_basis = counts;
        d.loss = Headline {
            value: counts.rate(),
            summary: None,
            basis: "UDP probes without an answer within 3 s, unloaded".into(),
        };
    }

    // TCP retransmissions on throughput connections.
    d.retrans = [
        retrans(s, Direction::Download),
        retrans(s, Direction::Upload),
    ];

    // Rolling windows for continuous runs.
    if continuous {
        for (label, span) in [
            ("Last 1 min", 60.0),
            ("Last 1 h", 3600.0),
            ("Whole run", f64::INFINITY),
        ] {
            let from = now - span;
            let all = span.is_infinite();
            let latency = if all {
                d.lat_by_load[0].summary
            } else {
                summarize_iter(scratch, idle_window(from).map(|x| x.ms))
            };
            let jit = if all {
                d.jitter_by_load[0]
            } else {
                jitter(idle_window(from).map(|x| x.ms))
            };
            let gateway = if all {
                d.gw_by_load[0].summary
            } else {
                let (a, b) = s.gateway.raw_range(from, f64::INFINITY);
                summarize_iter(
                    scratch,
                    s.gateway
                        .raw
                        .range(a, b)
                        .filter(|g| g.load == Load::Idle)
                        .filter_map(|g| g.ms),
                )
            };
            let loss = if all {
                let mut c = LossCounts::default();
                for x in s.loss_counts {
                    c.received += x.received;
                    c.lost += x.lost;
                    c.late += x.late;
                    c.duplicate += x.duplicate;
                }
                c
            } else {
                loss_counts(s, from)
            };
            d.windows.push(Window {
                label,
                latency,
                jitter: jit,
                loss,
                gateway,
            });
        }
    }
    d
}

fn deltas(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut prev: Option<f64> = None;
    let mut out = Vec::new();
    for v in values {
        if let Some(p) = prev {
            out.push((v - p).abs());
        }
        prev = Some(v);
    }
    out
}

fn loss_counts(s: &Session, from: f64) -> LossCounts {
    let (a, b) = s.loss.raw_range(from, f64::INFINITY);
    let mut c = LossCounts::default();
    for p in s.loss.raw.range(a, b) {
        match p.outcome {
            ProbeOutcome::Received => c.received += 1,
            ProbeOutcome::Lost => c.lost += 1,
            _ => {}
        }
    }
    // Late and duplicate answers are stored in arrival order; their send
    // times are close enough to sorted for a window boundary.
    let a = s.loss_odd.partition_point(|p| p.t < from);
    for p in s.loss_odd.range(a, s.loss_odd.len()) {
        match p.outcome {
            ProbeOutcome::Late => c.late += 1,
            ProbeOutcome::Duplicate => c.duplicate += 1,
            _ => {}
        }
    }
    c
}

/// Cumulative counters are per connection: take each connection's last
/// report and sum across connections.
fn retrans(s: &Session, dir: Direction) -> Option<Retrans> {
    let mut per_conn: BTreeMap<u32, Retrans> = BTreeMap::new();
    for r in s.requests_in(dir) {
        let e = per_conn.entry(r.conn).or_default();
        match dir {
            Direction::Download => {
                let Some(st) = r.server_tcp else { continue };
                if let (Some(sent), Some(re)) = (st.sent_packets, st.retrans_packets) {
                    e.packets = e.packets.max(sent);
                    e.retransmitted = e.retransmitted.max(re);
                }
                if let Some(l) = st.lost_packets {
                    e.lost = Some(e.lost.unwrap_or(0).max(l));
                }
            }
            Direction::Upload => {
                let Some(ct) = r.client_tcp else { continue };
                if let (Some(sent), Some(re)) = (ct.tx_packets, ct.tx_retrans_packets) {
                    e.packets = e.packets.max(sent);
                    e.retransmitted = e.retransmitted.max(re);
                }
            }
        }
    }
    let mut total = Retrans::default();
    let mut any = false;
    for v in per_conn.values() {
        if v.packets == 0 {
            continue;
        }
        any = true;
        total.packets += v.packets;
        total.retransmitted += v.retransmitted;
        if let Some(l) = v.lost {
            total.lost = Some(total.lost.unwrap_or(0) + l);
        }
    }
    any.then_some(total)
}
