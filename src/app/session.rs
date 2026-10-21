//! Everything measured in the current run, in bounded storage.

use std::time::{Instant, SystemTime};

use crate::model::{
    Config, ConnRecord, Direction, EdgeInfo, Event, GatewaySample, HostInfo, InterfaceInfo,
    LatencySample, Load, LossProbe, Phase, ProbeOutcome, RequestRecord, RunMode, Secs,
    SustainedRecord, ThroughputSample, WifiInfo, WifiSample,
};
use crate::stats::{LogHistogram, Ring, Sample, Series};

/// Raw-sample capacities. At continuous-mode defaults these hold roughly
/// two days of latency, one day of loss probes and 140 throughput cycles;
/// older data survives as minute/hour aggregates and histograms.
pub const RAW_LATENCY: usize = 200_000;
pub const RAW_LOSS: usize = 500_000;
pub const RAW_THROUGHPUT: usize = 200_000;
pub const RAW_GATEWAY: usize = 200_000;
pub const RAW_WIFI: usize = 100_000;
pub const RAW_REQUESTS: usize = 20_000;
pub const RAW_CONNS: usize = 10_000;
pub const RAW_ERRORS: usize = 2_000;
pub const RAW_LOSS_ODD: usize = 10_000;

impl Sample for ThroughputSample {
    fn t(&self) -> Secs {
        self.t
    }
    fn value(&self) -> Option<f64> {
        Some(self.bps)
    }
}

impl Sample for LatencySample {
    fn t(&self) -> Secs {
        self.t
    }
    fn value(&self) -> Option<f64> {
        Some(self.ms)
    }
}

impl Sample for LossProbe {
    fn t(&self) -> Secs {
        self.t
    }
    fn value(&self) -> Option<f64> {
        self.rtt_ms
    }
}

impl Sample for GatewaySample {
    fn t(&self) -> Secs {
        self.t
    }
    fn value(&self) -> Option<f64> {
        self.ms
    }
}

impl Sample for WifiSample {
    fn t(&self) -> Secs {
        self.t
    }
    fn value(&self) -> Option<f64> {
        self.rssi_dbm.map(f64::from)
    }
}

/// Running jitter (mean absolute difference between consecutive samples)
/// over a whole run, independent of how many raw samples are retained.
#[derive(Clone, Copy, Debug, Default)]
pub struct JitterAcc {
    last: Option<f64>,
    sum: f64,
    n: u64,
}

impl JitterAcc {
    fn add(&mut self, v: f64) {
        if let Some(p) = self.last {
            self.sum += (v - p).abs();
            self.n += 1;
        }
        self.last = Some(v);
    }

    pub fn value(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / self.n as f64)
    }
}

/// Probe counts for one load state.
#[derive(Clone, Copy, Debug, Default)]
pub struct LossCounts {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub duplicate: u64,
}

impl LossCounts {
    pub fn sent(&self) -> u64 {
        self.received + self.lost
    }
    pub fn rate(&self) -> Option<f64> {
        let s = self.sent();
        (s > 0).then(|| self.lost as f64 / s as f64)
    }
}

pub fn dir_idx(d: Direction) -> usize {
    match d {
        Direction::Download => 0,
        Direction::Upload => 1,
    }
}

fn load_idx(l: Load) -> usize {
    l.as_u8() as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
    Paused,
    Finished,
}

pub struct Session {
    pub mode: Option<RunMode>,
    pub config: Config,
    pub state: RunState,
    /// Wall-clock time and local `Instant` of the run's time origin.
    pub wall_start: Option<SystemTime>,
    pub local_start: Option<Instant>,
    pub finished_at: Option<Secs>,
    pub phase: Option<Phase>,
    pub progress: f32,
    pub host: Option<HostInfo>,
    pub edge: EdgeInfo,
    pub iface: Option<InterfaceInfo>,
    /// `Some(None)`: sampled, no Wi-Fi.
    pub wifi: Option<Option<WifiInfo>>,
    pub conns: Ring<ConnRecord>,
    pub requests: Ring<RequestRecord>,
    /// Sustained transfers; their streams' requests are in `requests`.
    pub sustained: Ring<SustainedRecord>,
    /// Fine-grained bins, download and upload.
    pub throughput: [Series<ThroughputSample>; 2],
    pub latency: Series<LatencySample>,
    /// All-run latency histograms per load state.
    pub latency_hist: [LogHistogram; 3],
    /// All-run jitter per load state.
    pub latency_jitter: [JitterAcc; 3],
    pub loss: Series<LossProbe>,
    pub loss_counts: [LossCounts; 3],
    /// Late and duplicate answers, which arrive out of time order.
    pub loss_odd: Ring<LossProbe>,
    pub gateway: Series<GatewaySample>,
    pub gateway_hist: [LogHistogram; 3],
    pub wifi_samples: Series<WifiSample>,
    pub errors: Ring<(Secs, String)>,
    pub pauses: Ring<(Secs, Option<Secs>)>,
    /// Bumped on every change, so views can cache derived values.
    pub version: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            mode: None,
            config: Config::default(),
            state: RunState::Idle,
            wall_start: None,
            local_start: None,
            finished_at: None,
            phase: None,
            progress: 0.0,
            host: None,
            edge: EdgeInfo::default(),
            iface: None,
            wifi: None,
            conns: Ring::new(RAW_CONNS),
            requests: Ring::new(RAW_REQUESTS),
            sustained: Ring::new(RAW_REQUESTS),
            throughput: [Series::new(RAW_THROUGHPUT), Series::new(RAW_THROUGHPUT)],
            latency: Series::new(RAW_LATENCY),
            latency_hist: Default::default(),
            latency_jitter: Default::default(),
            loss: Series::new(RAW_LOSS),
            loss_counts: Default::default(),
            loss_odd: Ring::new(RAW_LOSS_ODD),
            gateway: Series::new(RAW_GATEWAY),
            gateway_hist: Default::default(),
            wifi_samples: Series::new(RAW_WIFI),
            errors: Ring::new(RAW_ERRORS),
            pauses: Ring::new(10_000),
            version: 0,
        }
    }

    /// Drop all data, keeping the allocated capacity.
    pub fn clear(&mut self) {
        self.mode = None;
        self.state = RunState::Idle;
        self.wall_start = None;
        self.local_start = None;
        self.finished_at = None;
        self.phase = None;
        self.progress = 0.0;
        self.host = None;
        self.edge = EdgeInfo::default();
        self.iface = None;
        self.wifi = None;
        self.conns.clear();
        self.requests.clear();
        self.sustained.clear();
        for t in &mut self.throughput {
            t.clear();
        }
        self.latency.clear();
        self.latency_hist = Default::default();
        self.latency_jitter = Default::default();
        self.loss.clear();
        self.loss_counts = Default::default();
        self.loss_odd.clear();
        self.gateway.clear();
        self.gateway_hist = Default::default();
        self.wifi_samples.clear();
        self.errors.clear();
        self.pauses.clear();
        self.version += 1;
    }

    pub fn has_data(&self) -> bool {
        !self.requests.is_empty() || !self.latency.raw.is_empty() || !self.loss.raw.is_empty()
    }

    pub fn active(&self) -> bool {
        matches!(self.state, RunState::Running | RunState::Paused)
    }

    /// Seconds since the run's origin, by the local clock.
    pub fn now(&self) -> Option<Secs> {
        let end = self.finished_at;
        self.local_start
            .map(|s| s.elapsed().as_secs_f64())
            .map(|n| end.map_or(n, |e| n.min(e)))
    }

    pub fn wall_at(&self, t: Secs) -> Option<SystemTime> {
        self.wall_start
            .map(|w| w + std::time::Duration::from_secs_f64(t.max(0.0)))
    }

    pub fn apply(&mut self, e: Event) {
        self.version += 1;
        match e {
            Event::RunStarted { mode, config, wall } => {
                self.clear();
                self.mode = Some(mode);
                self.config = config;
                self.state = RunState::Running;
                self.wall_start = Some(wall);
                self.local_start = Some(Instant::now());
            }
            Event::Phase(p) => self.phase = Some(p),
            Event::Progress(p) => self.progress = p,
            Event::Host(h) => self.host = Some(h),
            Event::Edge(e) => self.edge = e,
            Event::Interface(i) => self.iface = Some(i),
            Event::Wifi(w) => self.wifi = Some(w),
            Event::WifiSample(s) => self.wifi_samples.push(s),
            Event::Conn(c) => {
                self.conns.push(c);
            }
            Event::Request(r) => {
                self.requests.push(r);
            }
            Event::Sustained(r) => {
                self.sustained.push(r);
            }
            Event::Throughput(s) => self.throughput[dir_idx(s.dir)].push(s),
            Event::Latency(s) => {
                self.latency_hist[load_idx(s.load)].add(s.ms);
                self.latency_jitter[load_idx(s.load)].add(s.ms);
                self.latency.push(s);
            }
            Event::Loss(p) => {
                let c = &mut self.loss_counts[load_idx(p.load)];
                match p.outcome {
                    ProbeOutcome::Received => {
                        c.received += 1;
                        self.loss.push(p);
                    }
                    ProbeOutcome::Lost => {
                        c.lost += 1;
                        self.loss.push(p);
                    }
                    ProbeOutcome::Late => {
                        c.late += 1;
                        self.loss_odd.push(p);
                    }
                    ProbeOutcome::Duplicate => {
                        c.duplicate += 1;
                        self.loss_odd.push(p);
                    }
                }
            }
            Event::Gateway(g) => {
                if let Some(ms) = g.ms {
                    self.gateway_hist[load_idx(g.load)].add(ms);
                }
                self.gateway.push(g);
            }
            Event::Error { t, message } => {
                self.errors.push((t, message));
            }
            Event::Reconfigured(cfg) => self.config = cfg,
            Event::Paused(t) => {
                if self.state != RunState::Finished {
                    self.state = RunState::Paused;
                    self.pauses.push((t, None));
                }
            }
            Event::Resumed(t) => {
                if self.state == RunState::Paused {
                    self.state = RunState::Running;
                }
                if let Some(p) = self.pauses.last_mut() {
                    p.1.get_or_insert(t);
                }
            }
            Event::Finished(t) => {
                self.state = RunState::Finished;
                self.finished_at = Some(t);
                if let Some(p) = self.pauses.last_mut() {
                    p.1.get_or_insert(t);
                }
                if !matches!(self.phase, Some(Phase::BackingOff { .. })) {
                    self.phase = None;
                }
            }
        }
    }

    /// The request with `id`; ids increase with start time.
    pub fn request(&self, id: u32) -> Option<&RequestRecord> {
        // Parallel streams finish out of id order, so look around the
        // binary search's answer.
        let i = self.requests.partition_point(|r| r.id < id);
        let near = i.saturating_sub(64)..(i + 64).min(self.requests.len());
        near.filter_map(|j| self.requests.get(j))
            .find(|r| r.id == id)
    }

    pub fn sustained(&self, id: u32) -> Option<&SustainedRecord> {
        let i = self.sustained.partition_point(|r| r.id < id);
        self.sustained.get(i).filter(|r| r.id == id)
    }

    /// The transfer whose bins carry `id`.
    pub fn transfer(&self, id: u32) -> Option<Transfer<'_>> {
        self.sustained(id).map(Transfer::Sustained).or_else(|| {
            self.request(id)
                .filter(|r| r.sustained.is_none())
                .map(Transfer::Single)
        })
    }

    /// Transfers overlapping `[t0, t1]` plus `margin` more on each side, in
    /// start order.
    pub fn transfers(&self, t0: Secs, t1: Secs, margin: usize) -> Transfers<'_> {
        let range = |n: usize, a: usize, b: usize| (a.saturating_sub(margin), (b + margin).min(n));
        let (sa, sb) = range(
            self.sustained.len(),
            self.sustained.partition_point(|r| r.t_end < t0),
            self.sustained.partition_point(|r| r.t_start <= t1),
        );
        let (ra, rb) = range(
            self.requests.len(),
            self.requests.partition_point(|r| r.t_end < t0),
            self.requests.partition_point(|r| r.t_start <= t1),
        );
        Transfers {
            s: self,
            sus: sa..sb,
            req: ra..rb,
        }
    }

    /// Requests that ran on their own, not as a stream of a sustained
    /// transfer.
    pub fn single_requests(&self) -> impl DoubleEndedIterator<Item = &RequestRecord> + Clone {
        self.requests.iter().filter(|r| r.sustained.is_none())
    }

    pub fn requests_in(
        &self,
        dir: Direction,
    ) -> impl DoubleEndedIterator<Item = &RequestRecord> + Clone {
        self.requests.iter().filter(move |r| r.dir == dir)
    }
}

/// What the throughput charts plot as one unit: a sustained transfer (its
/// streams' bytes summed), or a request that ran on its own. Its bins
/// carry its id.
#[derive(Clone, Copy, Debug)]
pub enum Transfer<'a> {
    Sustained(&'a SustainedRecord),
    Single(&'a RequestRecord),
}

impl Transfer<'_> {
    pub fn dir(&self) -> Direction {
        match self {
            Transfer::Sustained(r) => r.dir,
            Transfer::Single(r) => r.dir,
        }
    }
    pub fn t_start(&self) -> Secs {
        match self {
            Transfer::Sustained(r) => r.t_start,
            Transfer::Single(r) => r.t_start,
        }
    }
    pub fn t_end(&self) -> Secs {
        match self {
            Transfer::Sustained(r) => r.t_end,
            Transfer::Single(r) => r.t_end,
        }
    }
    pub fn bps(&self) -> Option<f64> {
        match self {
            Transfer::Sustained(r) => r.bps,
            Transfer::Single(r) => r.bps,
        }
    }
}

/// Sustained transfers and single requests merged in start order.
pub struct Transfers<'a> {
    s: &'a Session,
    sus: std::ops::Range<usize>,
    req: std::ops::Range<usize>,
}

impl<'a> Iterator for Transfers<'a> {
    type Item = Transfer<'a>;

    fn next(&mut self) -> Option<Transfer<'a>> {
        // Streams' requests are skipped: their sustained transfer stands
        // for them.
        while let Some(r) = self.s.requests.get(self.req.start) {
            if self.req.is_empty() || r.sustained.is_none() {
                break;
            }
            self.req.start += 1;
        }
        let sus = (!self.sus.is_empty())
            .then(|| self.s.sustained.get(self.sus.start))
            .flatten();
        let req = (!self.req.is_empty())
            .then(|| self.s.requests.get(self.req.start))
            .flatten();
        match (sus, req) {
            (Some(a), Some(b)) if b.t_start < a.t_start => {
                self.req.start += 1;
                Some(Transfer::Single(b))
            }
            (Some(a), _) => {
                self.sus.start += 1;
                Some(Transfer::Sustained(a))
            }
            (None, Some(b)) => {
                self.req.start += 1;
                Some(Transfer::Single(b))
            }
            (None, None) => None,
        }
    }
}

/// A session filled with `secs` of continuous-mode events at default
/// settings, for soak and performance tests. Values are arbitrary.
#[cfg(test)]
pub fn synthetic(secs: f64) -> Session {
    use crate::model::{Config, RunMode};
    let mut s = Session::new();
    s.apply(Event::RunStarted {
        mode: RunMode::Continuous,
        config: Config::default(),
        wall: SystemTime::now() - std::time::Duration::from_secs_f64(secs),
    });
    s.local_start = Some(Instant::now() - std::time::Duration::from_secs_f64(secs));
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut id = 1u32;
    let mut seq = 0u32;
    let mut t = 0.0;
    let mut next_cycle = 10.0;
    while t < secs {
        // One second: a latency probe, a gateway ping, five UDP probes and
        // every other second a Wi-Fi sample.
        s.apply(Event::Latency(LatencySample {
            t,
            ms: 18.0 + 6.0 * rnd() + if rnd() < 0.01 { 200.0 * rnd() } else { 0.0 },
            ttfb_ms: 45.0,
            server_ms: Some(25.0),
            load: Load::Idle,
            server_rtt_ms: Some(16.0),
            conn: 1,
            index: 0,
            count: 0,
        }));
        s.apply(Event::Gateway(GatewaySample {
            t: t + 0.1,
            ms: (rnd() > 0.002).then(|| 3.0 + 4.0 * rnd()),
            load: Load::Idle,
        }));
        for k in 0..5 {
            let lost = rnd() < 0.003;
            s.apply(Event::Loss(LossProbe {
                t: t + f64::from(k) * 0.2,
                seq,
                outcome: if lost {
                    ProbeOutcome::Lost
                } else {
                    ProbeOutcome::Received
                },
                rtt_ms: (!lost).then(|| 12.0 + 5.0 * rnd()),
                load: Load::Idle,
            }));
            seq += 1;
        }
        if (t as u64).is_multiple_of(2) {
            s.apply(Event::WifiSample(WifiSample {
                t,
                rssi_dbm: Some(-55 - (rnd() * 10.0) as i32),
                noise_dbm: Some(-95),
                tx_rate_mbps: Some(780.0),
            }));
        }
        if t >= next_cycle {
            // A 5 s sustained transfer per direction on 4 connections:
            // 100 summed bins, then each stream's request, then the record.
            let mut tc = t;
            for dir in [Direction::Download, Direction::Upload] {
                let sid = id;
                id += 1;
                let base = if dir == Direction::Download {
                    300e6
                } else {
                    40e6
                };
                let mut bytes = 0.0;
                let mut steady = 0.0;
                for bin in 0..100u32 {
                    let ramp = (f64::from(bin + 1) / 20.0).min(1.0);
                    let bps = base * ramp * (0.85 + 0.3 * rnd());
                    bytes += bps * 0.05 / 8.0;
                    if bin >= 40 {
                        steady += bps * 0.05 / 8.0;
                    }
                    s.apply(Event::Throughput(ThroughputSample {
                        t: tc + f64::from(bin + 1) * 0.05,
                        dt: 0.05,
                        bps,
                        dir,
                        request: sid,
                        bin,
                    }));
                }
                for index in 1..=4u32 {
                    let rid = id;
                    id += 1;
                    s.apply(Event::Request(RequestRecord {
                        id: rid,
                        conn: 2 + index,
                        new_conn: true,
                        sustained: Some(sid),
                        dir,
                        size: 99_999_999,
                        bytes: (bytes / 4.0) as u64,
                        index,
                        count: 4,
                        t_start: tc,
                        t_end: tc + 5.0,
                        ttfb_ms: 40.0,
                        transfer_ms: 4960.0,
                        server_ms: Some(20.0),
                        duration_ms: 4980.0,
                        bps: Some(base / 4.0),
                        complete: false,
                        server_tcp: None,
                        client_tcp: None,
                    }));
                }
                s.apply(Event::Sustained(SustainedRecord {
                    id: sid,
                    dir,
                    streams: 4,
                    t_start: tc,
                    t_end: tc + 5.0,
                    bytes: bytes as u64,
                    t_steady: tc + 2.0,
                    steady_bytes: steady as u64,
                    bps: Some(steady * 8.0 / 3.0),
                    requests: 4,
                    complete: true,
                }));
                tc += 5.0;
            }
            next_cycle += 300.0;
        }
        t += 1.0;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sustained transfers stand for their streams, and merge with single
    /// requests in start order; the headline is the latest transfer's rate.
    #[test]
    fn transfers_merge_and_set_the_headline() {
        let s = synthetic(1200.0);
        let all: Vec<Transfer<'_>> = s.transfers(0.0, 1200.0, 0).collect();
        assert_eq!(all.len(), s.sustained.len());
        assert!(all.windows(2).all(|w| w[0].t_start() <= w[1].t_start()));
        assert!(all.iter().all(|x| matches!(x, Transfer::Sustained(_))));
        let last = s
            .sustained
            .iter()
            .rev()
            .find(|r| r.dir == Direction::Download)
            .expect("a download transfer");
        assert!(matches!(s.transfer(last.id), Some(Transfer::Sustained(r)) if r.id == last.id));
        let stream = s
            .requests
            .iter()
            .find(|r| r.sustained == Some(last.id))
            .expect("its streams");
        assert!(s.transfer(stream.id).is_none(), "a stream isn't a transfer");
        let mut c = crate::app::derived::Cache::new();
        c.update(&s);
        assert_eq!(c.derived.download.value, last.bps);
        let sm = c.derived.download.summary.expect("bin statistics");
        assert_eq!(sm.n, 60, "bins after the 2 s ramp-up");
    }

    fn probe(t: f64, outcome: ProbeOutcome) -> Event {
        Event::Loss(LossProbe {
            t,
            seq: t as u32,
            outcome,
            rtt_ms: (outcome != ProbeOutcome::Lost).then_some(10.0),
            load: Load::Idle,
        })
    }

    #[test]
    fn counts_loss() {
        let mut s = Session::new();
        s.apply(probe(1.0, ProbeOutcome::Received));
        s.apply(probe(2.0, ProbeOutcome::Lost));
        s.apply(probe(2.0, ProbeOutcome::Late));
        s.apply(probe(3.0, ProbeOutcome::Received));
        let c = s.loss_counts[0];
        assert_eq!(c.sent(), 3);
        assert_eq!(c.lost, 1);
        assert_eq!(c.late, 1);
        assert_eq!(s.loss.raw.len(), 3);
        assert_eq!(s.loss_odd.len(), 1);
    }
}

#[cfg(test)]
mod memory {
    use super::*;
    use crate::stats::Bucket;

    /// Worst-case heap held by a session once every buffer is full.
    #[test]
    fn bounded_footprint() {
        use std::mem::size_of;
        let raw = RAW_LATENCY * size_of::<LatencySample>()
            + RAW_LOSS * size_of::<LossProbe>()
            + 2 * RAW_THROUGHPUT * size_of::<ThroughputSample>()
            + RAW_GATEWAY * size_of::<GatewaySample>()
            + RAW_WIFI * size_of::<WifiSample>()
            + RAW_REQUESTS * size_of::<RequestRecord>()
            + RAW_CONNS * (size_of::<ConnRecord>() + 64)
            + RAW_ERRORS * (size_of::<(Secs, String)>() + 80)
            + RAW_LOSS_ODD * size_of::<LossProbe>();
        // Six series, each with 30 days of minutes and a year of hours,
        // plus a histogram.
        let tiers = 6 * ((30 * 24 * 60 + 366 * 24) * size_of::<Bucket>() + 3_500 * 4);
        let total = raw + tiers;
        eprintln!(
            "raw {:.1} MB, aggregates {:.1} MB, total {:.1} MB",
            raw as f64 / 1e6,
            tiers as f64 / 1e6,
            total as f64 / 1e6
        );
        assert!(total < 120_000_000);
    }
}
