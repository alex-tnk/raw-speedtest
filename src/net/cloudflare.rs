//! Every Cloudflare endpoint the app talks to, and the parsing of what they
//! return. The endpoints are public but undocumented: nothing here assumes a
//! response shape without checking it, and every field is optional.

use super::json::Json;
use crate::model::{EdgeInfo, ServerTcp, Sourced};

pub const HOST: &str = "speed.cloudflare.com";
pub const PORT: u16 = 443;

/// Public STUN server on Cloudflare's anycast edge. Each Binding request
/// gets one Binding response, which makes it a UDP echo for loss and RTT.
pub const STUN_HOST: &str = "stun.cloudflare.com";
pub const STUN_PORT: u16 = 3478;

/// `__down` answers HTTP 403 for `bytes >= 100_000_000`.
pub const MAX_DOWNLOAD_BYTES: u64 = 99_999_999;

pub fn down_path(bytes: u64) -> String {
    format!("/__down?bytes={bytes}")
}

pub const UP_PATH: &str = "/__up";
pub const TRACE_PATH: &str = "/cdn-cgi/trace";
/// Answers `{}` unless the request carries the web test's Referer. The app
/// does not forge that header; it asks once and uses whatever comes back.
pub const META_PATH: &str = "/meta";

pub const SRC_DOWN_HEADERS: &str = "speed.cloudflare.com/__down response headers";
pub const SRC_TRACE: &str = "speed.cloudflare.com/cdn-cgi/trace";
pub const SRC_META: &str = "speed.cloudflare.com/meta";
pub const SRC_STUN: &str = "stun.cloudflare.com:3478 STUN XOR-MAPPED-ADDRESS";

/// Server processing time from `Server-Timing`, in ms: `cfRequestDuration`
/// when present, otherwise the sum of all `cfSpeed*` entries (the rule
/// Cloudflare's own speed test applies).
pub fn server_time_ms<'a>(server_timing: impl Iterator<Item = &'a str>) -> Option<f64> {
    let mut speed_sum = 0.0;
    let mut have_speed = false;
    for header in server_timing {
        for entry in split_entries(header) {
            let mut parts = entry.split(';');
            let name = parts.next().unwrap_or("").trim();
            let dur = parts.find_map(|p| {
                let p = p.trim();
                p.strip_prefix("dur=")
                    .and_then(|v| v.trim().parse::<f64>().ok())
            });
            let Some(dur) = dur else { continue };
            let lname = name.to_ascii_lowercase();
            if lname == "cfrequestduration" || lname == "cfreqdur" {
                if dur > 0.01 {
                    return Some(dur);
                }
            } else if lname.starts_with("cfspeed") {
                speed_sum += dur;
                have_speed = true;
            }
        }
    }
    (have_speed && speed_sum > 0.01).then_some(speed_sum)
}

/// Split a Server-Timing header into entries on commas outside quotes.
fn split_entries(header: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, c) in header.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&header[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&header[start..]);
    out
}

/// TCP counters from the `cfL4` Server-Timing entry, e.g.
/// `cfL4;desc="?proto=TCP&rtt=15764&min_rtt=14656&...&lost=0&retrans=0"`.
pub fn server_tcp<'a>(server_timing: impl Iterator<Item = &'a str>) -> Option<ServerTcp> {
    for header in server_timing {
        for entry in split_entries(header) {
            let entry = entry.trim();
            if !entry.to_ascii_lowercase().starts_with("cfl4") {
                continue;
            }
            let desc = entry
                .split(';')
                .find_map(|p| p.trim().strip_prefix("desc="))?;
            let desc = desc.trim_matches('"');
            let query = desc.strip_prefix('?').unwrap_or(desc);
            let mut st = ServerTcp::default();
            let mut any = false;
            for kv in query.split('&') {
                let Some((k, v)) = kv.split_once('=') else {
                    continue;
                };
                let Ok(v) = v.parse::<u64>() else { continue };
                let slot = match k {
                    "rtt" => &mut st.rtt_us,
                    "min_rtt" => &mut st.min_rtt_us,
                    "rtt_var" => &mut st.rtt_var_us,
                    "sent" => &mut st.sent_packets,
                    "recv" => &mut st.recv_packets,
                    "lost" => &mut st.lost_packets,
                    "retrans" => &mut st.retrans_packets,
                    "sent_bytes" => &mut st.sent_bytes,
                    "recv_bytes" => &mut st.recv_bytes,
                    "delivery_rate" => &mut st.delivery_rate,
                    "cwnd" => &mut st.cwnd,
                    _ => continue,
                };
                *slot = Some(v);
                any = true;
            }
            return any.then_some(st);
        }
    }
    None
}

/// Client and data-center metadata Cloudflare attaches to `__down` responses.
pub fn edge_from_headers<'a>(mut header: impl FnMut(&str) -> Option<&'a str>) -> EdgeInfo {
    let src = SRC_DOWN_HEADERS;
    let s = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| Sourced::new(v, src))
    };
    let lat = header("latitude").map(str::trim).filter(|v| !v.is_empty());
    let lon = header("longitude").map(str::trim).filter(|v| !v.is_empty());
    EdgeInfo {
        client_ip: s(header("cf-meta-ip")),
        asn: header("asn")
            .map(str::trim)
            .filter(|v| v.parse::<u64>().is_ok())
            .map(|v| Sourced::new(format!("AS{v}"), src)),
        colo: s(header("colo")),
        client_city: s(header("city")),
        client_country: s(header("country")),
        client_coords: match (lat, lon) {
            (Some(a), Some(b)) => Some(Sourced::new(format!("{a}, {b}"), src)),
            _ => None,
        },
        timezone: s(header("timezone")),
        ..EdgeInfo::default()
    }
}

/// Parse `/cdn-cgi/trace`: one `key=value` per line.
pub fn parse_trace(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// Fold trace fields into `edge`, filling only what is still missing, except
/// protocol fields that only the trace reports.
pub fn merge_trace(edge: &mut EdgeInfo, trace: &[(String, String)]) {
    let get = |k: &str| {
        trace
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let src = SRC_TRACE;
    if edge.client_ip.is_none() {
        edge.client_ip = get("ip").map(|v| Sourced::new(v, src));
    }
    if edge.colo.is_none() {
        edge.colo = get("colo").map(|v| Sourced::new(v, src));
    }
    if edge.client_country.is_none() {
        edge.client_country = get("loc").map(|v| Sourced::new(v, src));
    }
    edge.http = get("http").map(|v| Sourced::new(v, src));
    edge.tls = get("tls").map(|v| Sourced::new(v, src));
    edge.kex = get("kex").map(|v| Sourced::new(v, src));
    edge.warp = get("warp").map(|v| Sourced::new(v, src));
}

/// Fold a `/meta` body into `edge` when it has the documented-by-use shape:
/// `{"asOrganization": "...", "region": "...", "colo": {"city": "..."}}`.
pub fn merge_meta(edge: &mut EdgeInfo, body: &str) {
    let Some(json) = Json::parse(body) else {
        return;
    };
    let src = SRC_META;
    if let Some(v) = json.get("asOrganization").and_then(Json::as_str) {
        edge.as_org = Some(Sourced::new(v, src));
    }
    if let Some(v) = json.get("region").and_then(Json::as_str) {
        edge.client_region = Some(Sourced::new(v, src));
    }
    if let Some(v) = json
        .get("colo")
        .and_then(|c| c.get("city"))
        .and_then(Json::as_str)
    {
        edge.colo_city = Some(Sourced::new(v, src));
    }
    if edge.asn.is_none()
        && let Some(v) = json.get("asn").and_then(Json::as_f64)
    {
        edge.asn = Some(Sourced::new(format!("AS{v}"), src));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ST: [&str; 2] = [
        "cfSpeedEdge;dur=3, cfSpeedWorker;dur=26",
        "cfL4;desc=\"?proto=TCP&rtt=15764&min_rtt=14656&rtt_var=6108&sent=5&recv=8&lost=0&retrans=2&sent_bytes=2838&recv_bytes=513&delivery_rate=182928&cwnd=53&unsent_bytes=0&cid=608be54da1d66b54&ts=58&x=0\"",
    ];

    #[test]
    fn server_time_sums_cf_speed() {
        assert_eq!(server_time_ms(ST.iter().copied()), Some(29.0));
        assert_eq!(
            server_time_ms(["cfRequestDuration;dur=12.5"].into_iter()),
            Some(12.5)
        );
        assert_eq!(server_time_ms(["other;dur=4"].into_iter()), None);
        assert_eq!(server_time_ms(std::iter::empty()), None);
    }

    #[test]
    fn cf_l4_parses() {
        let st = server_tcp(ST.iter().copied()).unwrap();
        assert_eq!(st.rtt_us, Some(15764));
        assert_eq!(st.min_rtt_us, Some(14656));
        assert_eq!(st.lost_packets, Some(0));
        assert_eq!(st.retrans_packets, Some(2));
        assert_eq!(st.cwnd, Some(53));
        assert_eq!(server_tcp(["cfSpeedEdge;dur=3"].into_iter()), None);
    }

    #[test]
    fn trace_and_meta_merge() {
        let t = parse_trace("fl=1\nip=1.2.3.4\ncolo=DFW\nhttp=http/1.1\ntls=TLSv1.3\nkex=X25519\n");
        let mut e = EdgeInfo::default();
        merge_trace(&mut e, &t);
        assert_eq!(e.client_ip.as_ref().unwrap().value, "1.2.3.4");
        assert_eq!(e.tls.as_ref().unwrap().value, "TLSv1.3");
        merge_meta(&mut e, "{}");
        assert!(e.as_org.is_none());
        merge_meta(
            &mut e,
            r#"{"asn":3354,"asOrganization":"Example Net","region":"Texas","colo":{"iata":"DFW","city":"Dallas-Fort Worth"}}"#,
        );
        assert_eq!(e.as_org.unwrap().value, "Example Net");
        assert_eq!(e.colo_city.unwrap().value, "Dallas-Fort Worth");
        assert_eq!(e.asn.unwrap().value, "AS3354");
    }

    #[test]
    fn headers_to_edge() {
        let h = |k: &str| match k {
            "cf-meta-ip" => Some("206.77.150.111"),
            "asn" => Some("3354"),
            "colo" => Some("DFW"),
            "latitude" => Some("30.2"),
            "longitude" => Some("-97.7"),
            _ => None,
        };
        let e = edge_from_headers(h);
        assert_eq!(e.asn.unwrap().value, "AS3354");
        assert_eq!(e.client_coords.unwrap().value, "30.2, -97.7");
        assert!(e.client_city.is_none());
    }
}
