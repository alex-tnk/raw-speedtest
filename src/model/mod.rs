//! Plain data types shared by the engine and the UI.
//!
//! Every timestamp is `Secs`: seconds since the run's monotonic origin
//! (`Instant` captured when the run started). The wall-clock time of that
//! origin travels with `Event::RunStarted` so the UI can label samples.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime};

pub type Secs = f64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Download,
    Upload,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Download => "Download",
            Direction::Upload => "Upload",
        }
    }
    pub fn lower(self) -> &'static str {
        match self {
            Direction::Download => "download",
            Direction::Upload => "upload",
        }
    }
}

/// What the link was doing while a latency or loss probe was in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Load {
    Idle,
    Download,
    Upload,
}

impl Load {
    pub fn from_u8(v: u8) -> Load {
        match v {
            1 => Load::Download,
            2 => Load::Upload,
            _ => Load::Idle,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            Load::Idle => 0,
            Load::Download => 1,
            Load::Upload => 2,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Load::Idle => "Unloaded",
            Load::Download => "During download",
            Load::Upload => "During upload",
        }
    }
    pub const ALL: [Load; 3] = [Load::Idle, Load::Download, Load::Upload];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    OneShot,
    Continuous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpFamily {
    Auto,
    V4,
    V6,
}

impl IpFamily {
    pub fn label(self) -> &'static str {
        match self {
            IpFamily::Auto => "Auto",
            IpFamily::V4 => "IPv4",
            IpFamily::V6 => "IPv6",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContinuousConfig {
    /// Time between the starts of two throughput cycles.
    pub cycle_interval: Duration,
    /// Length of each direction's sustained phase per cycle.
    pub sustain: Duration,
    /// Time between two idle latency probes.
    pub latency_interval: Duration,
    /// UDP loss probes per second.
    pub loss_rate_hz: f64,
}

impl Default for ContinuousConfig {
    fn default() -> Self {
        // Modest by default: on a 150/90 Mbps line this moves about
        // 0.6 GB of throughput data per hour.
        ContinuousConfig {
            cycle_interval: Duration::from_secs(900),
            sustain: Duration::from_secs(5),
            latency_interval: Duration::from_secs(1),
            loss_rate_hz: 5.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub family: IpFamily,
    /// Parallel connections in a sustained transfer.
    pub streams: u32,
    pub continuous: ContinuousConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            family: IpFamily::Auto,
            streams: 4,
            continuous: ContinuousConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    Start(RunMode, Config),
    Stop,
    Pause,
    Resume,
    /// Change the continuous-mode load while a run is active.
    Reconfigure(Config),
}

/// The step the engine is executing, for the status line and progress bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    Latency {
        done: u32,
        total: u32,
    },
    /// A sustained transfer started at `started` and lasting `length`.
    Sustained {
        dir: Direction,
        streams: u32,
        started: Secs,
        length: Secs,
    },
    PacketLoss {
        sent: u32,
        total: u32,
    },
    /// Continuous mode, between throughput cycles.
    Waiting {
        next_cycle_at: Secs,
    },
    BackingOff {
        until: Secs,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnPurpose {
    Metadata,
    Latency,
    LoadedLatency,
    Download,
    Upload,
}

impl ConnPurpose {
    pub fn label(self) -> &'static str {
        match self {
            ConnPurpose::Metadata => "Metadata",
            ConnPurpose::Latency => "Latency",
            ConnPurpose::LoadedLatency => "Loaded latency",
            ConnPurpose::Download => "Download",
            ConnPurpose::Upload => "Upload",
        }
    }
}

/// Setup timing of one TCP + TLS connection.
#[derive(Clone, Debug)]
pub struct ConnRecord {
    pub id: u32,
    pub t: Secs,
    pub purpose: ConnPurpose,
    pub remote: SocketAddr,
    pub local: SocketAddr,
    pub dns_ms: f64,
    pub connect_ms: f64,
    pub tls_ms: f64,
    pub tls_version: String,
    pub cipher: String,
    pub alpn: Option<String>,
}

/// TCP counters reported by Cloudflare's edge in the `cfL4` Server-Timing
/// entry: the server's view of this connection, cumulative at the moment the
/// response headers were generated.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ServerTcp {
    pub rtt_us: Option<u64>,
    pub min_rtt_us: Option<u64>,
    pub rtt_var_us: Option<u64>,
    pub sent_packets: Option<u64>,
    pub recv_packets: Option<u64>,
    pub lost_packets: Option<u64>,
    pub retrans_packets: Option<u64>,
    pub sent_bytes: Option<u64>,
    pub recv_bytes: Option<u64>,
    pub delivery_rate: Option<u64>,
    pub cwnd: Option<u64>,
}

/// TCP counters from this machine's kernel for one socket.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TcpStats {
    pub srtt_ms: Option<f64>,
    pub rttvar_ms: Option<f64>,
    pub rtt_cur_ms: Option<f64>,
    pub mss: Option<u32>,
    pub cwnd_bytes: Option<u64>,
    pub tx_packets: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub tx_retrans_packets: Option<u64>,
    pub tx_retrans_bytes: Option<u64>,
    pub rx_packets: Option<u64>,
    pub rx_bytes: Option<u64>,
    pub rx_ooo_bytes: Option<u64>,
    /// Bytes acknowledged by the peer, cumulative.
    pub bytes_acked: Option<u64>,
    /// Bytes in the send buffer, including unacknowledged in-flight bytes.
    pub send_queue_bytes: Option<u64>,
}

/// One completed (or truncated) download or upload request.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    pub id: u32,
    pub conn: u32,
    /// True when this request opened its connection.
    pub new_conn: bool,
    /// The sustained transfer this request was one stream of; its bytes
    /// are binned with the other streams' under that id.
    pub sustained: Option<u32>,
    pub dir: Direction,
    /// Requested payload size.
    pub size: u64,
    /// Body bytes actually transferred.
    pub bytes: u64,
    /// The stream it ran on (1-based) and the number of streams.
    pub index: u32,
    pub count: u32,
    pub t_start: Secs,
    pub t_end: Secs,
    /// Request start to first response byte.
    pub ttfb_ms: f64,
    /// Download: first to last body byte. Upload: first body byte written
    /// to response received.
    pub transfer_ms: f64,
    pub server_ms: Option<f64>,
    /// Duration the throughput is computed over (Cloudflare's definition).
    pub duration_ms: f64,
    pub bps: Option<f64>,
    /// False when the request hit the time limit before the full payload.
    pub complete: bool,
    pub server_tcp: Option<ServerTcp>,
    pub client_tcp: Option<TcpStats>,
}

/// A sustained transfer: several connections kept busy with back-to-back
/// requests in one direction for a fixed time. Its throughput bins carry
/// its `id` and sum every stream.
#[derive(Clone, Debug)]
pub struct SustainedRecord {
    pub id: u32,
    pub dir: Direction,
    pub streams: u32,
    pub t_start: Secs,
    pub t_end: Secs,
    /// Bytes across all streams (download: body bytes received; upload:
    /// bytes the server acknowledged).
    pub bytes: u64,
    /// Start of the steady part the throughput is computed over, after
    /// the connections have ramped up.
    pub t_steady: Secs,
    pub steady_bytes: u64,
    pub bps: Option<f64>,
    /// HTTP requests across all streams.
    pub requests: u32,
    /// False when it ended early (error, rate limit, stop).
    pub complete: bool,
}

/// Throughput over one short bin inside a request.
#[derive(Clone, Copy, Debug)]
pub struct ThroughputSample {
    /// End of the bin.
    pub t: Secs,
    /// Bin length.
    pub dt: f32,
    pub bps: f64,
    pub dir: Direction,
    /// The request, or the sustained transfer, the bin belongs to.
    pub request: u32,
    /// 0-based bin number within the request.
    pub bin: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct LatencySample {
    pub t: Secs,
    /// TTFB minus server processing time.
    pub ms: f64,
    pub ttfb_ms: f64,
    pub server_ms: Option<f64>,
    pub load: Load,
    /// Smoothed RTT the edge measured for this TCP connection.
    pub server_rtt_ms: Option<f64>,
    pub conn: u32,
    /// 1-based number within its burst (one-shot) or 0 for periodic probes.
    pub index: u32,
    pub count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    Received,
    Lost,
    /// Arrived after it had been declared lost.
    Late,
    Duplicate,
}

/// One UDP STUN probe.
#[derive(Clone, Copy, Debug)]
pub struct LossProbe {
    /// Send time.
    pub t: Secs,
    pub seq: u32,
    pub outcome: ProbeOutcome,
    pub rtt_ms: Option<f64>,
    pub load: Load,
}

#[derive(Clone, Copy, Debug)]
pub struct GatewaySample {
    pub t: Secs,
    pub ms: Option<f64>,
    pub load: Load,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiInfo {
    pub interface: Option<String>,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub rssi_dbm: Option<i32>,
    pub noise_dbm: Option<i32>,
    pub channel: Option<u32>,
    pub band: Option<&'static str>,
    pub width_mhz: Option<u32>,
    pub tx_rate_mbps: Option<f64>,
    pub phy_mode: Option<&'static str>,
    pub security: Option<&'static str>,
    pub country: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct WifiSample {
    pub t: Secs,
    pub rssi_dbm: Option<i32>,
    pub noise_dbm: Option<i32>,
    pub tx_rate_mbps: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InterfaceInfo {
    pub name: Option<String>,
    pub kind: Option<&'static str>,
    pub mtu: Option<u32>,
    pub local_ip: Option<IpAddr>,
    pub gateway: Option<IpAddr>,
}

/// A value plus where it came from, for "hover shows the raw source".
#[derive(Clone, Debug, PartialEq)]
pub struct Sourced {
    pub value: String,
    pub source: &'static str,
}

impl Sourced {
    pub fn new(value: impl Into<String>, source: &'static str) -> Sourced {
        Sourced {
            value: value.into(),
            source,
        }
    }
}

/// What Cloudflare reports about this client and the serving data center.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeInfo {
    pub client_ip: Option<Sourced>,
    pub asn: Option<Sourced>,
    pub as_org: Option<Sourced>,
    pub colo: Option<Sourced>,
    pub colo_city: Option<Sourced>,
    pub client_city: Option<Sourced>,
    pub client_region: Option<Sourced>,
    pub client_country: Option<Sourced>,
    pub client_coords: Option<Sourced>,
    pub timezone: Option<Sourced>,
    pub http: Option<Sourced>,
    pub tls: Option<Sourced>,
    pub kex: Option<Sourced>,
    pub warp: Option<Sourced>,
    pub udp_mapped: Option<Sourced>,
}

/// DNS answers and per-family reachability of the test host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostInfo {
    pub host: String,
    pub v4: Vec<IpAddr>,
    pub v6: Vec<IpAddr>,
    pub dns_ms: Option<f64>,
    pub v4_connect_ms: Option<Result<f64, String>>,
    pub v6_connect_ms: Option<Result<f64, String>>,
    pub used: Option<SocketAddr>,
}

#[derive(Clone, Debug)]
pub enum Event {
    RunStarted {
        mode: RunMode,
        config: Config,
        wall: SystemTime,
    },
    Phase(Phase),
    /// Fraction of the one-shot plan completed, 0..=1.
    Progress(f32),
    Host(HostInfo),
    Edge(EdgeInfo),
    Interface(InterfaceInfo),
    Wifi(Option<WifiInfo>),
    WifiSample(WifiSample),
    Conn(ConnRecord),
    Request(RequestRecord),
    Sustained(SustainedRecord),
    Throughput(ThroughputSample),
    Latency(LatencySample),
    Loss(LossProbe),
    Gateway(GatewaySample),
    Error {
        t: Secs,
        message: String,
    },
    /// Continuous-mode settings changed during the run.
    Reconfigured(Config),
    Paused(Secs),
    Resumed(Secs),
    Finished(Secs),
}
