//! Blocking HTTP/1.1 over TCP + rustls with a timestamp at every step a
//! measurement needs: DNS, TCP connect, TLS handshake, request sent, first
//! response byte (the socket read that delivered it), last body byte.
//!
//! Sockets use 100 ms timeouts so every loop can notice an abort (pause or
//! stop) promptly without a second thread.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};

use crate::model::IpFamily;

const POLL: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Give up when no byte moves for this long.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEAD: usize = 32 * 1024;
const MAX_TEXT_BODY: usize = 256 * 1024;
const USER_AGENT: &str = concat!("raw-speed-test/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, PartialEq)]
pub enum NetError {
    /// Pause or stop requested.
    Aborted,
    Dns(String),
    NoAddress(&'static str),
    Connect(String),
    Tls(String),
    Io(String),
    Stalled,
    Http {
        status: u16,
        retry_after: Option<Duration>,
    },
    Protocol(String),
    Closed,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Aborted => write!(f, "aborted"),
            NetError::Dns(e) => write!(f, "DNS lookup failed: {e}"),
            NetError::NoAddress(fam) => write!(f, "no {fam} address"),
            NetError::Connect(e) => write!(f, "TCP connect failed: {e}"),
            NetError::Tls(e) => write!(f, "TLS failed: {e}"),
            NetError::Io(e) => write!(f, "{e}"),
            NetError::Stalled => write!(f, "no data for {} s", STALL_TIMEOUT.as_secs()),
            NetError::Http { status, .. } => write!(f, "HTTP {status}"),
            NetError::Protocol(e) => write!(f, "bad response: {e}"),
            NetError::Closed => write!(f, "connection closed by server"),
        }
    }
}

fn io_err(e: io::Error) -> NetError {
    NetError::Io(e.to_string())
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Shared TLS configuration: Mozilla roots compiled in, HTTP/1.1 only, and
/// no session resumption so every handshake is a full one and handshake
/// times are comparable.
pub fn tls_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        cfg.resumption = rustls::client::Resumption::disabled();
        Arc::new(cfg)
    })
    .clone()
}

pub struct Resolved {
    pub addrs: Vec<SocketAddr>,
    pub elapsed: Duration,
}

/// Resolve with the system resolver (getaddrinfo), timed.
pub fn resolve(host: &str, port: u16) -> Result<Resolved, NetError> {
    let t = Instant::now();
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| NetError::Dns(e.to_string()))?
        .collect();
    let elapsed = t.elapsed();
    if addrs.is_empty() {
        return Err(NetError::Dns("no addresses".into()));
    }
    Ok(Resolved { addrs, elapsed })
}

/// Pick the address to use: the resolver's first choice for `Auto`.
pub fn pick(addrs: &[SocketAddr], family: IpFamily) -> Result<SocketAddr, NetError> {
    let found = match family {
        IpFamily::Auto => addrs.first(),
        IpFamily::V4 => addrs.iter().find(|a| a.is_ipv4()),
        IpFamily::V6 => addrs.iter().find(|a| a.is_ipv6() && !is_v4_mapped(a.ip())),
    };
    found.copied().ok_or(NetError::NoAddress(family.label()))
}

fn is_v4_mapped(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some())
}

/// Plain TCP connect, timed.
pub fn tcp_connect(addr: SocketAddr) -> Result<(TcpStream, Duration), NetError> {
    let t = Instant::now();
    let sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| NetError::Connect(e.to_string()))?;
    Ok((sock, t.elapsed()))
}

/// Response status line and headers.
#[derive(Clone, Debug, Default)]
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|v| v.trim().parse().ok())
    }

    fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    }

    fn closes(&self) -> bool {
        self.header("connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
    }

    fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
    }
}

/// Timestamps of one request/response exchange.
#[derive(Clone, Debug)]
pub struct Exchange {
    /// Just before the first request byte was handed to TLS.
    pub t_start: Instant,
    /// After the last request byte was written to the socket.
    pub t_sent: Instant,
    /// The socket read that delivered the first response byte.
    pub t_first_byte: Instant,
    /// The socket read that delivered the last body byte.
    pub t_end: Instant,
    pub head: Head,
    pub body_bytes: u64,
    /// False when the body was cut short by the caller's deadline.
    pub complete: bool,
}

/// One keep-alive TLS connection.
pub struct Conn {
    pub remote: SocketAddr,
    pub local: SocketAddr,
    pub dns: Duration,
    pub connect: Duration,
    pub tls_handshake: Duration,
    sock: TcpStream,
    tls: ClientConnection,
    /// Plaintext already decrypted but not yet consumed.
    pending: Vec<u8>,
    pending_pos: usize,
    /// When the most recent socket read returned data.
    last_read: Instant,
    /// TLS bytes written to the socket, total.
    pub wire_written: u64,
    /// TLS bytes read from the socket, total.
    pub wire_read: u64,
    pub requests: u32,
    usable: bool,
}

/// Something that can say "stop now".
pub type Abort<'a> = &'a dyn Fn() -> bool;

impl Conn {
    /// Resolve, connect and handshake, timing each step.
    pub fn open(host: &str, port: u16, family: IpFamily, abort: Abort) -> Result<Conn, NetError> {
        let r = resolve(host, port)?;
        let addr = pick(&r.addrs, family)?;
        if abort() {
            return Err(NetError::Aborted);
        }
        let (sock, connect) = tcp_connect(addr)?;
        Self::handshake(host, sock, r.elapsed, connect, abort)
    }

    fn handshake(
        host: &str,
        sock: TcpStream,
        dns: Duration,
        connect: Duration,
        abort: Abort,
    ) -> Result<Conn, NetError> {
        sock.set_nodelay(true).map_err(io_err)?;
        sock.set_read_timeout(Some(POLL)).map_err(io_err)?;
        sock.set_write_timeout(Some(POLL)).map_err(io_err)?;
        let remote = sock.peer_addr().map_err(io_err)?;
        let local = sock.local_addr().map_err(io_err)?;
        let name =
            ServerName::try_from(host.to_owned()).map_err(|e| NetError::Tls(e.to_string()))?;
        let tls =
            ClientConnection::new(tls_config(), name).map_err(|e| NetError::Tls(e.to_string()))?;
        let now = Instant::now();
        let mut c = Conn {
            remote,
            local,
            dns,
            connect,
            tls_handshake: Duration::ZERO,
            sock,
            tls,
            pending: Vec::with_capacity(64 * 1024),
            pending_pos: 0,
            last_read: now,
            wire_written: 0,
            wire_read: 0,
            requests: 0,
            usable: true,
        };
        let t = Instant::now();
        let stall = Stall::new();
        while c.tls.is_handshaking() {
            c.flush(abort, &stall)?;
            if c.tls.is_handshaking() && c.tls.wants_read() {
                c.read_tls(abort, &stall)?;
            }
        }
        c.tls_handshake = t.elapsed();
        c.flush(abort, &stall)?;
        Ok(c)
    }

    pub fn socket(&self) -> &TcpStream {
        &self.sock
    }

    pub fn usable(&self) -> bool {
        self.usable
    }

    pub fn tls_version(&self) -> String {
        match self.tls.protocol_version() {
            Some(rustls::ProtocolVersion::TLSv1_3) => "TLS 1.3".into(),
            Some(rustls::ProtocolVersion::TLSv1_2) => "TLS 1.2".into(),
            Some(v) => format!("{v:?}"),
            None => String::new(),
        }
    }

    pub fn cipher(&self) -> String {
        self.tls
            .negotiated_cipher_suite()
            .map(|s| format!("{:?}", s.suite()))
            .unwrap_or_default()
    }

    pub fn alpn(&self) -> Option<String> {
        self.tls
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned())
    }

    /// Write queued TLS records to the socket.
    fn flush(&mut self, abort: Abort, stall: &Stall) -> Result<(), NetError> {
        while self.tls.wants_write() {
            match self.tls.write_tls(&mut self.sock) {
                Ok(n) => {
                    self.wire_written += n as u64;
                    stall.moved(abort).map_err(|e| self.fail(e))?;
                }
                Err(e) if is_timeout(&e) => stall.check(abort).map_err(|e| self.fail(e))?,
                Err(e) => return Err(self.fail(io_err(e))),
            }
        }
        Ok(())
    }

    /// One socket read into rustls. Returns once some bytes arrived.
    fn read_tls(&mut self, abort: Abort, stall: &Stall) -> Result<(), NetError> {
        loop {
            match self.tls.read_tls(&mut self.sock) {
                Ok(0) => return Err(self.fail(NetError::Closed)),
                Ok(n) => {
                    self.last_read = Instant::now();
                    self.wire_read += n as u64;
                    if let Err(e) = self.tls.process_new_packets() {
                        return Err(self.fail(NetError::Tls(e.to_string())));
                    }
                    stall.moved(abort).map_err(|e| self.fail(e))?;
                    return Ok(());
                }
                Err(e) if is_timeout(&e) => stall.check(abort).map_err(|e| self.fail(e))?,
                Err(e) => return Err(self.fail(io_err(e))),
            }
        }
    }

    fn fail(&mut self, e: NetError) -> NetError {
        self.usable = false;
        e
    }

    /// Ensure decrypted plaintext is available in `pending`.
    /// Returns the time of the socket read that delivered it.
    fn fill(&mut self, abort: Abort, stall: &Stall) -> Result<Instant, NetError> {
        if self.pending_pos < self.pending.len() {
            return Ok(self.last_read);
        }
        self.pending.clear();
        self.pending_pos = 0;
        loop {
            self.pending.resize(64 * 1024, 0);
            let r = self.tls.reader().read(&mut self.pending);
            match r {
                Ok(0) => {
                    self.pending.clear();
                    return Err(self.fail(NetError::Closed));
                }
                Ok(n) => {
                    self.pending.truncate(n);
                    return Ok(self.last_read);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.pending.clear();
                    if self.tls.wants_write() {
                        self.flush(abort, stall)?;
                    }
                    self.read_tls(abort, stall)?;
                }
                Err(e) => {
                    self.pending.clear();
                    return Err(self.fail(io_err(e)));
                }
            }
        }
    }

    fn write_plain(
        &mut self,
        mut data: &[u8],
        abort: Abort,
        stall: &Stall,
    ) -> Result<(), NetError> {
        while !data.is_empty() {
            let n = self.tls.writer().write(data).map_err(io_err)?;
            data = &data[n..];
            self.flush(abort, stall)?;
        }
        Ok(())
    }

    fn request_head(method: &str, path: &str, body_len: Option<u64>) -> String {
        let mut s = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nConnection: keep-alive\r\n",
            super::cloudflare::HOST
        );
        if let Some(n) = body_len {
            s.push_str(&format!(
                "Content-Type: application/octet-stream\r\nContent-Length: {n}\r\n"
            ));
        }
        s.push_str("\r\n");
        s
    }

    /// Read the status line and headers. Returns the time of the socket
    /// read that delivered the first response byte.
    fn read_head(&mut self, abort: Abort, stall: &Stall) -> Result<(Head, Instant), NetError> {
        let mut buf: Vec<u8> = Vec::with_capacity(1024);
        let mut first: Option<Instant> = None;
        loop {
            let t = self.fill(abort, stall)?;
            first.get_or_insert(t);
            let avail = &self.pending[self.pending_pos..];
            // Search from a little before the new data in case "\r\n\r\n"
            // straddles two reads.
            let search_from = buf.len().saturating_sub(3);
            buf.extend_from_slice(avail);
            if let Some(end) = find(&buf[search_from..], b"\r\n\r\n").map(|i| i + search_from + 4) {
                let used = end - (buf.len() - avail.len());
                self.pending_pos += used;
                buf.truncate(end);
                let head = parse_head(&buf).map_err(|e| self.fail(e))?;
                return Ok((head, first.expect("set above")));
            }
            self.pending_pos = self.pending.len();
            if buf.len() > MAX_HEAD {
                return Err(self.fail(NetError::Protocol("headers too large".into())));
            }
        }
    }

    /// Read the response body, calling `on_data(t, total_body_bytes)` after
    /// every socket read. Stops early (marking the exchange incomplete) at
    /// `deadline`. Returns `(bytes, t_last, complete)`.
    fn read_body(
        &mut self,
        head: &Head,
        abort: Abort,
        stall: &Stall,
        deadline: Option<Instant>,
        mut keep: Option<&mut Vec<u8>>,
        on_data: &mut dyn FnMut(Instant, u64),
    ) -> Result<(u64, Instant, bool), NetError> {
        let mut total: u64 = 0;
        let mut t_last = self.last_read;
        let mut take = |c: &mut Conn,
                        max: u64,
                        total: &mut u64,
                        t_last: &mut Instant|
         -> Result<u64, NetError> {
            let t = c.fill(abort, stall)?;
            let avail = (c.pending.len() - c.pending_pos) as u64;
            let n = avail.min(max);
            if let Some(k) = keep.as_deref_mut() {
                if k.len() + (n as usize) > MAX_TEXT_BODY {
                    return Err(c.fail(NetError::Protocol("body too large".into())));
                }
                k.extend_from_slice(&c.pending[c.pending_pos..c.pending_pos + n as usize]);
            }
            c.pending_pos += n as usize;
            *total += n;
            *t_last = t;
            Ok(n)
        };
        if head.chunked() {
            loop {
                let line = self.read_line(abort, stall)?;
                let size_str = line.split(';').next().unwrap_or("").trim();
                let size = u64::from_str_radix(size_str, 16)
                    .map_err(|_| self.fail(NetError::Protocol("bad chunk size".into())))?;
                if size == 0 {
                    // Trailers until an empty line.
                    while !self.read_line(abort, stall)?.is_empty() {}
                    break;
                }
                let mut left = size;
                while left > 0 {
                    left -= take(self, left, &mut total, &mut t_last)?;
                    on_data(t_last, total);
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        self.usable = false;
                        return Ok((total, t_last, false));
                    }
                }
                let crlf = self.read_line(abort, stall)?;
                if !crlf.is_empty() {
                    return Err(self.fail(NetError::Protocol("bad chunk end".into())));
                }
            }
        } else if let Some(len) = head.content_length() {
            while total < len {
                take(self, len - total, &mut total, &mut t_last)?;
                on_data(t_last, total);
                if total < len && deadline.is_some_and(|d| Instant::now() >= d) {
                    self.usable = false;
                    return Ok((total, t_last, false));
                }
            }
        } else if head.status != 204 && head.status != 304 {
            // Body runs to connection close.
            loop {
                match take(self, u64::MAX, &mut total, &mut t_last) {
                    Ok(_) => on_data(t_last, total),
                    Err(NetError::Closed) => break,
                    Err(e) => return Err(e),
                }
            }
            self.usable = false;
        }
        if head.closes() {
            self.usable = false;
        }
        Ok((total, t_last, true))
    }

    fn read_line(&mut self, abort: Abort, stall: &Stall) -> Result<String, NetError> {
        let mut line = Vec::new();
        loop {
            self.fill(abort, stall)?;
            let avail = &self.pending[self.pending_pos..];
            if let Some(i) = avail.iter().position(|&b| b == b'\n') {
                line.extend_from_slice(&avail[..i]);
                self.pending_pos += i + 1;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            line.extend_from_slice(avail);
            self.pending_pos = self.pending.len();
            if line.len() > 4096 {
                return Err(self.fail(NetError::Protocol("line too long".into())));
            }
        }
    }

    fn check_status(head: &Head) -> Result<(), NetError> {
        if (200..300).contains(&head.status) {
            Ok(())
        } else {
            Err(NetError::Http {
                status: head.status,
                retry_after: head.retry_after(),
            })
        }
    }

    /// GET whose body is counted but discarded. `on_data(t, bytes)` sees
    /// every socket read of the body.
    pub fn get(
        &mut self,
        path: &str,
        abort: Abort,
        deadline: Option<Instant>,
        on_data: &mut dyn FnMut(Instant, u64),
    ) -> Result<Exchange, NetError> {
        self.exchange_get(path, abort, deadline, None, on_data)
    }

    /// GET returning a (small) body.
    pub fn get_text(&mut self, path: &str, abort: Abort) -> Result<(Exchange, String), NetError> {
        let mut body = Vec::new();
        let ex = self.exchange_get(path, abort, None, Some(&mut body), &mut |_, _| {})?;
        Ok((ex, String::from_utf8_lossy(&body).into_owned()))
    }

    /// Any failure other than an HTTP status leaves the connection in an
    /// unknown state: it is not reused.
    fn settle<T>(&mut self, r: Result<T, NetError>) -> Result<T, NetError> {
        if let Err(e) = &r
            && !matches!(e, NetError::Http { .. })
        {
            self.usable = false;
        }
        r
    }

    fn exchange_get(
        &mut self,
        path: &str,
        abort: Abort,
        deadline: Option<Instant>,
        keep: Option<&mut Vec<u8>>,
        on_data: &mut dyn FnMut(Instant, u64),
    ) -> Result<Exchange, NetError> {
        let r = self.exchange_get_inner(path, abort, deadline, keep, on_data);
        self.settle(r)
    }

    fn exchange_get_inner(
        &mut self,
        path: &str,
        abort: Abort,
        deadline: Option<Instant>,
        keep: Option<&mut Vec<u8>>,
        on_data: &mut dyn FnMut(Instant, u64),
    ) -> Result<Exchange, NetError> {
        if !self.usable {
            return Err(NetError::Closed);
        }
        self.requests += 1;
        let stall = Stall::new();
        let req = Self::request_head("GET", path, None);
        let t_start = Instant::now();
        self.write_plain(req.as_bytes(), abort, &stall)?;
        let t_sent = Instant::now();
        let (head, t_first_byte) = self.read_head(abort, &stall)?;
        let ok = Self::check_status(&head);
        let mut sink = Vec::new();
        let (body_bytes, t_end, complete) = match (ok.is_ok(), keep) {
            (true, keep) => self.read_body(&head, abort, &stall, deadline, keep, on_data)?,
            // Drain error bodies so the connection stays reusable.
            (false, _) => {
                self.read_body(&head, abort, &stall, None, Some(&mut sink), &mut |_, _| {})?
            }
        };
        ok?;
        Ok(Exchange {
            t_start,
            t_sent,
            t_first_byte,
            t_end,
            head,
            body_bytes,
            complete,
        })
    }

    /// POST `len` bytes of incompressible data. `on_sent(t, socket_bytes,
    /// conn)` runs after every socket write of the body so the caller can
    /// sample kernel TCP counters. When `deadline` passes before the body is
    /// fully sent, the request is abandoned and `Ok(None)` returned with the
    /// connection unusable.
    pub fn post(
        &mut self,
        path: &str,
        len: u64,
        abort: Abort,
        deadline: Option<Instant>,
        on_sent: &mut dyn FnMut(Instant, u64, &TcpStream),
    ) -> Result<Result<Exchange, Instant>, NetError> {
        let r = self.post_inner(path, len, abort, deadline, on_sent);
        self.settle(r)
    }

    fn post_inner(
        &mut self,
        path: &str,
        len: u64,
        abort: Abort,
        deadline: Option<Instant>,
        on_sent: &mut dyn FnMut(Instant, u64, &TcpStream),
    ) -> Result<Result<Exchange, Instant>, NetError> {
        if !self.usable {
            return Err(NetError::Closed);
        }
        self.requests += 1;
        let stall = Stall::new();
        let req = Self::request_head("POST", path, Some(len));
        let body = upload_body();
        let t_start = Instant::now();
        self.write_plain(req.as_bytes(), abort, &stall)?;
        let wire_base = self.wire_written;
        let mut left = len;
        let mut off = 0usize;
        while left > 0 {
            let n = (body.len() - off).min(left as usize).min(64 * 1024);
            let chunk = &body[off..off + n];
            let mut data = chunk;
            while !data.is_empty() {
                let w = self.tls.writer().write(data).map_err(io_err)?;
                data = &data[w..];
                self.flush(abort, &stall)?;
                on_sent(Instant::now(), self.wire_written - wire_base, &self.sock);
            }
            left -= n as u64;
            off = (off + n) % body.len();
            if left > 0 && deadline.is_some_and(|d| Instant::now() >= d) {
                self.usable = false;
                return Ok(Err(Instant::now()));
            }
        }
        let t_sent = Instant::now();
        let (head, t_first_byte) = self.read_head(abort, &stall)?;
        let ok = Self::check_status(&head);
        let mut sink = Vec::new();
        let (_, t_end, _) =
            self.read_body(&head, abort, &stall, None, Some(&mut sink), &mut |_, _| {})?;
        ok?;
        Ok(Ok(Exchange {
            t_start,
            t_sent,
            t_first_byte,
            t_end,
            head,
            body_bytes: len,
            complete: true,
        }))
    }
}

/// Tracks when bytes last moved, for the stall timeout and abort checks.
struct Stall {
    last: std::cell::Cell<Instant>,
    /// When `abort` was last asked while data was flowing.
    polled: std::cell::Cell<Instant>,
}

impl Stall {
    fn new() -> Stall {
        Stall {
            last: std::cell::Cell::new(Instant::now()),
            polled: std::cell::Cell::new(Instant::now()),
        }
    }

    /// Bytes moved. Also asks `abort` every `POLL / 2`: while a transfer
    /// streams, socket calls never time out and would otherwise never see
    /// a pause or stop.
    fn moved(&self, abort: Abort) -> Result<(), NetError> {
        let now = Instant::now();
        self.last.set(now);
        if now.saturating_duration_since(self.polled.get()) >= POLL / 2 {
            self.polled.set(now);
            if abort() {
                return Err(NetError::Aborted);
            }
        }
        Ok(())
    }
    fn check(&self, abort: Abort) -> Result<(), NetError> {
        if abort() {
            return Err(NetError::Aborted);
        }
        if self.last.get().elapsed() > STALL_TIMEOUT {
            return Err(NetError::Stalled);
        }
        Ok(())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_head(buf: &[u8]) -> Result<Head, NetError> {
    let text =
        std::str::from_utf8(buf).map_err(|_| NetError::Protocol("non-UTF-8 headers".into()))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(NetError::Protocol(format!("status line {status_line:?}")));
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| NetError::Protocol(format!("status line {status_line:?}")))?;
    let headers = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Ok(Head { status, headers })
}

/// 1 MiB of pseudo-random bytes, generated once. Random content keeps any
/// compressing middlebox (some VPNs) from inflating upload numbers.
fn upload_body() -> &'static [u8] {
    static BODY: OnceLock<Vec<u8>> = OnceLock::new();
    BODY.get_or_init(|| {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut v = Vec::with_capacity(1 << 20);
        while v.len() < (1 << 20) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_head() {
        let h = parse_head(
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nserver-timing: a;dur=1\r\nServer-Timing: b;dur=2\r\n\r\n",
        )
        .unwrap();
        assert_eq!(h.status, 429);
        assert_eq!(h.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(h.all("Server-Timing").count(), 2);
        assert!(parse_head(b"SSH-2.0\r\n\r\n").is_err());
    }

    #[test]
    fn picks_family() {
        let v4: SocketAddr = "1.2.3.4:443".parse().unwrap();
        let v6: SocketAddr = "[2606:4700::1]:443".parse().unwrap();
        assert_eq!(pick(&[v6, v4], IpFamily::Auto), Ok(v6));
        assert_eq!(pick(&[v6, v4], IpFamily::V4), Ok(v4));
        assert_eq!(pick(&[v4], IpFamily::V6), Err(NetError::NoAddress("IPv6")));
    }

    #[test]
    fn body_is_random_looking() {
        let b = upload_body();
        assert_eq!(b.len(), 1 << 20);
        let zeros = b.iter().filter(|&&x| x == 0).count();
        assert!(zeros < b.len() / 100);
    }
}
