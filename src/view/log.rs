//! Every request, connection and error, newest first, as table rows.

use super::format;
use super::palette::Color;
use super::tip::Tip;
use crate::app::session::Session;
use crate::engine::size_label;
use crate::model::Direction;

use super::chart::series::dir_color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tab {
    Requests = 0,
    Connections = 1,
    Errors = 2,
}

impl Tab {
    pub fn from_u8(v: u8) -> Option<Tab> {
        [Tab::Requests, Tab::Connections, Tab::Errors]
            .get(usize::from(v))
            .copied()
    }

    /// Tab label with its row count: `Requests 1,204`.
    pub fn label(self, n: usize) -> String {
        let name = match self {
            Tab::Requests => "Requests",
            Tab::Connections => "Connections",
            Tab::Errors => "Errors",
        };
        format!("{name} {}", format::count(n as u64))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// Title with its unit: `TTFB ms`.
    pub title: &'static str,
    /// Numeric columns are right-aligned.
    pub numeric: bool,
    /// Preferred width in points.
    pub width: f32,
}

const fn col(title: &'static str, numeric: bool, width: f32) -> Column {
    Column {
        title,
        numeric,
        width,
    }
}

const REQUESTS: [Column; 11] = [
    col("Time", false, 92.0),
    col("Test", false, 72.0),
    col("Size", true, 56.0),
    col("Sample", true, 56.0),
    col("Conn", true, 72.0),
    col("TTFB ms", true, 72.0),
    col("Server ms", true, 72.0),
    col("Transfer ms", true, 80.0),
    col("Duration ms", true, 80.0),
    col("Throughput Mbps", true, 96.0),
    col("Retrans", true, 64.0),
];

const CONNECTIONS: [Column; 9] = [
    col("Time", false, 92.0),
    col("Use", false, 104.0),
    col("Remote", false, 150.0),
    col("Local", false, 150.0),
    col("DNS ms", true, 64.0),
    col("TCP ms", true, 64.0),
    col("TLS ms", true, 64.0),
    col("Version", false, 64.0),
    col("Cipher", false, 220.0),
];

const ERRORS: [Column; 2] = [col("Time", false, 104.0), col("Message", false, 600.0)];

pub fn columns(tab: Tab) -> &'static [Column] {
    match tab {
        Tab::Requests => &REQUESTS,
        Tab::Connections => &CONNECTIONS,
        Tab::Errors => &ERRORS,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Stable while the row is held: its position since the run started.
    pub id: u64,
    pub cells: Vec<Cell>,
    pub tip: Option<Tip>,
}

pub fn len(s: &Session, tab: Tab) -> usize {
    match tab {
        Tab::Requests => s.requests.len(),
        Tab::Connections => s.conns.len(),
        Tab::Errors => s.errors.len(),
    }
}

fn cell(text: impl Into<String>) -> Cell {
    Cell {
        text: text.into(),
        color: Color::Text,
    }
}

fn missing() -> Cell {
    Cell {
        text: super::frame::MISSING.into(),
        color: Color::Text2,
    }
}

fn opt_ms(v: Option<f64>) -> Cell {
    match v {
        Some(v) if v.is_finite() => cell(format::ms(v)),
        _ => missing(),
    }
}

/// Rows `first..first + count` of `tab`, newest first.
pub fn rows(s: &Session, tab: Tab, first: usize, count: usize) -> Vec<Row> {
    let n = len(s, tab);
    let wall = |t: f64| {
        s.wall_at(t)
            .map(|w| format::clock(w, true))
            .unwrap_or_default()
    };
    let evicted = match tab {
        Tab::Requests => s.requests.evicted(),
        Tab::Connections => s.conns.evicted(),
        Tab::Errors => s.errors.evicted(),
    };
    (first..(first + count).min(n))
        .filter_map(|i| {
            let k = n - 1 - i;
            let id = evicted + k as u64;
            match tab {
                Tab::Requests => s.requests.get(k).map(|r| request(id, r, &wall)),
                Tab::Connections => s.conns.get(k).map(|c| Row {
                    id,
                    cells: vec![
                        cell(wall(c.t)),
                        cell(c.purpose.label()),
                        cell(c.remote.to_string()),
                        cell(c.local.to_string()),
                        opt_ms(Some(c.dns_ms)),
                        opt_ms(Some(c.connect_ms)),
                        opt_ms(Some(c.tls_ms)),
                        cell(c.tls_version.clone()),
                        cell(c.cipher.clone()),
                    ],
                    tip: Some(
                        Tip::default()
                            .title(format!("Connection #{}", c.id))
                            .pair("ALPN", c.alpn.clone().unwrap_or_else(|| super::frame::MISSING.into()))
                            .note("DNS: getaddrinfo. TCP: connect() to the first address of the chosen family. TLS: ClientHello sent to handshake complete (full handshake, no resumption)."),
                    ),
                }),
                Tab::Errors => s.errors.get(k).map(|(t, m)| Row {
                    id,
                    cells: vec![
                        cell(wall(*t)),
                        Cell {
                            text: m.clone(),
                            color: Color::Error,
                        },
                    ],
                    tip: None,
                }),
            }
        })
        .collect()
}

fn request(id: u64, r: &crate::model::RequestRecord, wall: &dyn Fn(f64) -> String) -> Row {
    let color = dir_color(r.dir);
    let retrans = match r.dir {
        Direction::Download => r.server_tcp.and_then(|t| t.retrans_packets),
        Direction::Upload => r.client_tcp.and_then(|t| t.tx_retrans_packets),
    };
    let cells = vec![
        cell(wall(r.t_start)),
        Cell {
            text: r.dir.label().into(),
            color,
        },
        cell(size_label(r.size)),
        cell(format!("{}/{}", r.index, r.count)),
        cell(format!(
            "#{}{}",
            r.conn,
            if r.new_conn { " new" } else { "" }
        )),
        opt_ms(Some(r.ttfb_ms)),
        opt_ms(r.server_ms),
        opt_ms(Some(r.transfer_ms)),
        opt_ms(Some(r.duration_ms)),
        match r.bps {
            Some(b) => Cell {
                text: format::mbps(b),
                color,
            },
            None => missing(),
        },
        match retrans {
            Some(v) => cell(format::count(v)),
            None => missing(),
        },
    ];
    let mut t = Tip::default().title(format!(
        "{} {} · sample {}/{}",
        size_label(r.size),
        r.dir.lower(),
        r.index,
        r.count
    ));
    t.push_pair("Bytes", format::count(r.bytes));
    if !r.complete {
        t.push_pair("Complete", "No, time limit");
    }
    if let Some(st) = r.server_tcp {
        if let Some(v) = st.rtt_us {
            t.push_pair(
                "Edge TCP RTT",
                format!("{} ms", format::ms(v as f64 / 1000.0)),
            );
        }
        if let Some(v) = st.cwnd {
            t.push_pair("Edge cwnd", format!("{} packets", format::count(v)));
        }
        if let Some(v) = st.sent_packets {
            t.push_pair("Edge packets sent", format::count(v));
        }
        if let Some(v) = st.lost_packets {
            t.push_pair("Edge packets lost", format::count(v));
        }
        if let Some(v) = st.retrans_packets {
            t.push_pair("Edge retransmits", format::count(v));
        }
    }
    if let Some(ct) = r.client_tcp {
        if let Some(v) = ct.srtt_ms {
            t.push_pair("Local TCP RTT", format!("{} ms", format::ms(v)));
        }
        if let Some(v) = ct.mss {
            t.push_pair("MSS", format!("{v} bytes"));
        }
        if let Some(v) = ct.tx_retrans_packets {
            t.push_pair("Local retransmits", format::count(v));
        }
        if let Some(v) = ct.rx_ooo_bytes {
            t.push_pair("Out-of-order bytes", format::count(v));
        }
    }
    Row {
        id,
        cells,
        tip: Some(t.note(
            "Edge counters come from the response's cfL4 Server-Timing entry and are cumulative for the connection when the headers were sent. Local counters are this machine's kernel TCP statistics after the request.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_newest_first_and_match_columns() {
        let s = crate::app::session::synthetic(1800.0);
        for tab in [Tab::Requests, Tab::Connections, Tab::Errors] {
            let n = len(&s, tab);
            let rows = rows(&s, tab, 0, 50);
            assert_eq!(rows.len(), n.min(50));
            for r in &rows {
                assert_eq!(r.cells.len(), columns(tab).len(), "{tab:?}");
            }
        }
        let first = rows(&s, Tab::Requests, 0, 1);
        let newest = s.requests.last().expect("requests");
        assert_eq!(
            first[0].cells[3].text,
            format!("{}/{}", newest.index, newest.count)
        );
        assert!(rows(&s, Tab::Requests, len(&s, Tab::Requests), 10).is_empty());
    }
}
