//! JSON export of every raw sample the session holds. Written only to a
//! path the user picked.

use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use super::session::Session;
use crate::model::{EdgeInfo, Load, ProbeOutcome, ServerTcp, Sourced, TcpStats};
use crate::net::json::escape;

fn num(out: &mut String, v: f64) {
    if v.is_finite() {
        let _ = write!(out, "{v}");
    } else {
        out.push_str("null");
    }
}

fn opt(out: &mut String, v: Option<f64>) {
    match v {
        Some(v) => num(out, v),
        None => out.push_str("null"),
    }
}

fn opt_u(out: &mut String, v: Option<u64>) {
    match v {
        Some(v) => {
            let _ = write!(out, "{v}");
        }
        None => out.push_str("null"),
    }
}

fn s(out: &mut String, v: &str) {
    escape(v, out);
}

fn load(l: Load) -> &'static str {
    match l {
        Load::Idle => "idle",
        Load::Download => "download",
        Load::Upload => "upload",
    }
}

fn sourced(out: &mut String, key: &str, v: &Option<Sourced>) {
    s(out, key);
    out.push(':');
    match v {
        Some(v) => {
            out.push_str("{\"value\":");
            s(out, &v.value);
            out.push_str(",\"source\":");
            s(out, v.source);
            out.push('}');
        }
        None => out.push_str("null"),
    }
}

fn edge(out: &mut String, e: &EdgeInfo) {
    out.push('{');
    let fields: [(&str, &Option<Sourced>); 15] = [
        ("client_ip", &e.client_ip),
        ("asn", &e.asn),
        ("as_org", &e.as_org),
        ("colo", &e.colo),
        ("colo_city", &e.colo_city),
        ("client_city", &e.client_city),
        ("client_region", &e.client_region),
        ("client_country", &e.client_country),
        ("client_coords", &e.client_coords),
        ("timezone", &e.timezone),
        ("http", &e.http),
        ("tls", &e.tls),
        ("kex", &e.kex),
        ("warp", &e.warp),
        ("udp_mapped", &e.udp_mapped),
    ];
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        sourced(out, k, v);
    }
    out.push('}');
}

fn server_tcp(out: &mut String, t: &Option<ServerTcp>) {
    let Some(t) = t else {
        out.push_str("null");
        return;
    };
    out.push_str("{\"rtt_us\":");
    opt_u(out, t.rtt_us);
    out.push_str(",\"min_rtt_us\":");
    opt_u(out, t.min_rtt_us);
    out.push_str(",\"rtt_var_us\":");
    opt_u(out, t.rtt_var_us);
    out.push_str(",\"sent_packets\":");
    opt_u(out, t.sent_packets);
    out.push_str(",\"recv_packets\":");
    opt_u(out, t.recv_packets);
    out.push_str(",\"lost_packets\":");
    opt_u(out, t.lost_packets);
    out.push_str(",\"retrans_packets\":");
    opt_u(out, t.retrans_packets);
    out.push_str(",\"sent_bytes\":");
    opt_u(out, t.sent_bytes);
    out.push_str(",\"recv_bytes\":");
    opt_u(out, t.recv_bytes);
    out.push_str(",\"delivery_rate\":");
    opt_u(out, t.delivery_rate);
    out.push_str(",\"cwnd\":");
    opt_u(out, t.cwnd);
    out.push('}');
}

fn client_tcp(out: &mut String, t: &Option<TcpStats>) {
    let Some(t) = t else {
        out.push_str("null");
        return;
    };
    out.push_str("{\"srtt_ms\":");
    opt(out, t.srtt_ms);
    out.push_str(",\"rttvar_ms\":");
    opt(out, t.rttvar_ms);
    out.push_str(",\"mss\":");
    opt_u(out, t.mss.map(u64::from));
    out.push_str(",\"cwnd_bytes\":");
    opt_u(out, t.cwnd_bytes);
    out.push_str(",\"tx_packets\":");
    opt_u(out, t.tx_packets);
    out.push_str(",\"tx_bytes\":");
    opt_u(out, t.tx_bytes);
    out.push_str(",\"tx_retrans_packets\":");
    opt_u(out, t.tx_retrans_packets);
    out.push_str(",\"tx_retrans_bytes\":");
    opt_u(out, t.tx_retrans_bytes);
    out.push_str(",\"rx_packets\":");
    opt_u(out, t.rx_packets);
    out.push_str(",\"rx_bytes\":");
    opt_u(out, t.rx_bytes);
    out.push_str(",\"rx_ooo_bytes\":");
    opt_u(out, t.rx_ooo_bytes);
    out.push('}');
}

/// Serialize the session. Times are seconds since `run_start_unix`.
pub fn to_json(ses: &Session) -> String {
    let mut o = String::with_capacity(1 << 20);
    o.push_str("{\"format\":\"raw-speed-test/1\",\"run_start_unix\":");
    opt(
        &mut o,
        ses.wall_start
            .and_then(|w| w.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64()),
    );
    o.push_str(",\"mode\":");
    s(
        &mut o,
        match ses.mode {
            Some(crate::model::RunMode::OneShot) => "one-shot",
            Some(crate::model::RunMode::Continuous) => "continuous",
            None => "none",
        },
    );
    // Headline results with how each is computed.
    let mut cache = super::derived::Cache::new();
    cache.update(ses);
    let d = &cache.derived;
    o.push_str(",\"summary\":{");
    for (i, (key, h)) in [
        ("download_bps", &d.download),
        ("upload_bps", &d.upload),
        ("latency_ms", &d.latency),
        ("jitter_ms", &d.jitter),
        ("loss_ratio", &d.loss),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            o.push(',');
        }
        s(&mut o, key);
        o.push_str(":{\"value\":");
        opt(&mut o, h.value);
        o.push_str(",\"basis\":");
        s(&mut o, &h.basis);
        o.push('}');
    }
    o.push('}');
    o.push_str(",\"edge\":");
    edge(&mut o, &ses.edge);

    o.push_str(",\"host\":");
    match &ses.host {
        Some(h) => {
            o.push_str("{\"name\":");
            s(&mut o, &h.host);
            o.push_str(",\"dns_ms\":");
            opt(&mut o, h.dns_ms);
            o.push_str(",\"ipv4\":[");
            for (i, a) in h.v4.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                s(&mut o, &a.to_string());
            }
            o.push_str("],\"ipv6\":[");
            for (i, a) in h.v6.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                s(&mut o, &a.to_string());
            }
            o.push_str("]}");
        }
        None => o.push_str("null"),
    }

    o.push_str(",\"interface\":");
    match &ses.iface {
        Some(i) => {
            o.push_str("{\"name\":");
            match &i.name {
                Some(n) => s(&mut o, n),
                None => o.push_str("null"),
            }
            o.push_str(",\"type\":");
            match i.kind {
                Some(k) => s(&mut o, k),
                None => o.push_str("null"),
            }
            o.push_str(",\"mtu\":");
            opt_u(&mut o, i.mtu.map(u64::from));
            o.push_str(",\"gateway\":");
            match i.gateway {
                Some(g) => s(&mut o, &g.to_string()),
                None => o.push_str("null"),
            }
            o.push('}');
        }
        None => o.push_str("null"),
    }

    o.push_str(",\"connections\":[");
    for (i, c) in ses.conns.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let _ = write!(o, "{{\"id\":{},\"t\":", c.id);
        num(&mut o, c.t);
        o.push_str(",\"purpose\":");
        s(&mut o, c.purpose.label());
        o.push_str(",\"remote\":");
        s(&mut o, &c.remote.to_string());
        o.push_str(",\"local\":");
        s(&mut o, &c.local.to_string());
        o.push_str(",\"dns_ms\":");
        num(&mut o, c.dns_ms);
        o.push_str(",\"connect_ms\":");
        num(&mut o, c.connect_ms);
        o.push_str(",\"tls_ms\":");
        num(&mut o, c.tls_ms);
        o.push_str(",\"tls_version\":");
        s(&mut o, &c.tls_version);
        o.push_str(",\"cipher\":");
        s(&mut o, &c.cipher);
        o.push('}');
    }

    o.push_str("],\"requests\":[");
    for (i, r) in ses.requests.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let _ = write!(
            o,
            "{{\"id\":{},\"conn\":{},\"new_conn\":{},\"direction\":\"{}\",\"size\":{},\"bytes\":{},\"index\":{},\"count\":{},\"t_start\":",
            r.id,
            r.conn,
            r.new_conn,
            r.dir.lower(),
            r.size,
            r.bytes,
            r.index,
            r.count
        );
        num(&mut o, r.t_start);
        o.push_str(",\"t_end\":");
        num(&mut o, r.t_end);
        o.push_str(",\"ttfb_ms\":");
        num(&mut o, r.ttfb_ms);
        o.push_str(",\"transfer_ms\":");
        num(&mut o, r.transfer_ms);
        o.push_str(",\"server_ms\":");
        opt(&mut o, r.server_ms);
        o.push_str(",\"duration_ms\":");
        num(&mut o, r.duration_ms);
        o.push_str(",\"bps\":");
        opt(&mut o, r.bps);
        let _ = write!(o, ",\"complete\":{},\"server_tcp\":", r.complete);
        server_tcp(&mut o, &r.server_tcp);
        o.push_str(",\"client_tcp\":");
        client_tcp(&mut o, &r.client_tcp);
        o.push('}');
    }

    o.push_str("],\"throughput\":[");
    for (i, x) in ses.throughput[0]
        .raw
        .iter()
        .chain(ses.throughput[1].raw.iter())
        .enumerate()
    {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"t\":");
        num(&mut o, x.t);
        o.push_str(",\"dt\":");
        num(&mut o, f64::from(x.dt));
        o.push_str(",\"bps\":");
        num(&mut o, x.bps);
        let _ = write!(
            o,
            ",\"direction\":\"{}\",\"request\":{},\"bin\":{}}}",
            x.dir.lower(),
            x.request,
            x.bin
        );
    }

    o.push_str("],\"latency\":[");
    for (i, x) in ses.latency.raw.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"t\":");
        num(&mut o, x.t);
        o.push_str(",\"ms\":");
        num(&mut o, x.ms);
        o.push_str(",\"ttfb_ms\":");
        num(&mut o, x.ttfb_ms);
        o.push_str(",\"server_ms\":");
        opt(&mut o, x.server_ms);
        o.push_str(",\"server_rtt_ms\":");
        opt(&mut o, x.server_rtt_ms);
        let _ = write!(o, ",\"load\":\"{}\",\"conn\":{}}}", load(x.load), x.conn);
    }

    o.push_str("],\"udp_probes\":[");
    let mut first = true;
    for x in ses.loss.raw.iter().chain(ses.loss_odd.iter()) {
        if !first {
            o.push(',');
        }
        first = false;
        let _ = write!(o, "{{\"t\":");
        num(&mut o, x.t);
        let outcome = match x.outcome {
            ProbeOutcome::Received => "received",
            ProbeOutcome::Lost => "lost",
            ProbeOutcome::Late => "late",
            ProbeOutcome::Duplicate => "duplicate",
        };
        let _ = write!(
            o,
            ",\"seq\":{},\"outcome\":\"{outcome}\",\"rtt_ms\":",
            x.seq
        );
        opt(&mut o, x.rtt_ms);
        let _ = write!(o, ",\"load\":\"{}\"}}", load(x.load));
    }

    o.push_str("],\"gateway\":[");
    for (i, x) in ses.gateway.raw.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"t\":");
        num(&mut o, x.t);
        o.push_str(",\"ms\":");
        opt(&mut o, x.ms);
        let _ = write!(o, ",\"load\":\"{}\"}}", load(x.load));
    }

    o.push_str("],\"wifi\":[");
    for (i, x) in ses.wifi_samples.raw.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"t\":");
        num(&mut o, x.t);
        o.push_str(",\"rssi_dbm\":");
        opt(&mut o, x.rssi_dbm.map(f64::from));
        o.push_str(",\"noise_dbm\":");
        opt(&mut o, x.noise_dbm.map(f64::from));
        o.push_str(",\"tx_rate_mbps\":");
        opt(&mut o, x.tx_rate_mbps);
        o.push('}');
    }

    o.push_str("],\"errors\":[");
    for (i, (t, m)) in ses.errors.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"t\":");
        num(&mut o, *t);
        o.push_str(",\"message\":");
        s(&mut o, m);
        o.push('}');
    }
    o.push_str("]}");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::json::Json;

    #[test]
    fn empty_session_is_valid_json() {
        let j = to_json(&Session::new());
        let v = Json::parse(&j).expect("valid JSON");
        assert_eq!(
            v.get("format").and_then(Json::as_str),
            Some("raw-speed-test/1")
        );
    }
}
