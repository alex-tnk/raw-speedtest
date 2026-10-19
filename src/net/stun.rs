//! STUN Binding requests (RFC 8489) as a UDP echo: one request, one
//! response, matched by transaction ID.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;

/// Transaction ID layout: 4-byte sequence number, 8-byte session nonce.
pub fn request(seq: u32, nonce: u64) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    // Message length 0: no attributes.
    m[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    m[8..12].copy_from_slice(&seq.to_be_bytes());
    m[12..20].copy_from_slice(&nonce.to_be_bytes());
    m
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub seq: u32,
    pub mapped: Option<SocketAddr>,
}

/// Parse a Binding success response belonging to `nonce`.
pub fn parse(buf: &[u8], nonce: u64) -> Option<Response> {
    if buf.len() < 20 {
        return None;
    }
    let typ = u16::from_be_bytes([buf[0], buf[1]]);
    let len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
    let magic = u32::from_be_bytes(buf[4..8].try_into().ok()?);
    if typ != BINDING_SUCCESS || magic != MAGIC || buf.len() < 20 + len {
        return None;
    }
    if u64::from_be_bytes(buf[12..20].try_into().ok()?) != nonce {
        return None;
    }
    let seq = u32::from_be_bytes(buf[8..12].try_into().ok()?);
    let tid: [u8; 12] = buf[8..20].try_into().ok()?;
    let mut mapped = None;
    let mut attrs = &buf[20..20 + len];
    while attrs.len() >= 4 {
        let at = u16::from_be_bytes([attrs[0], attrs[1]]);
        let al = usize::from(u16::from_be_bytes([attrs[2], attrs[3]]));
        let Some(val) = attrs.get(4..4 + al) else {
            break;
        };
        match at {
            ATTR_XOR_MAPPED_ADDRESS => mapped = address(val, true, &tid).or(mapped),
            ATTR_MAPPED_ADDRESS if mapped.is_none() => mapped = address(val, false, &tid),
            _ => {}
        }
        let padded = (al + 3) & !3;
        attrs = attrs.get(4 + padded..).unwrap_or(&[]);
    }
    Some(Response { seq, mapped })
}

fn address(v: &[u8], xor: bool, tid: &[u8; 12]) -> Option<SocketAddr> {
    if v.len() < 4 {
        return None;
    }
    let family = v[1];
    let mut port = u16::from_be_bytes([v[2], v[3]]);
    let magic = MAGIC.to_be_bytes();
    if xor {
        port ^= (MAGIC >> 16) as u16;
    }
    match family {
        0x01 if v.len() >= 8 => {
            let mut b: [u8; 4] = v[4..8].try_into().ok()?;
            if xor {
                for (x, m) in b.iter_mut().zip(magic) {
                    *x ^= m;
                }
            }
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(b)), port))
        }
        0x02 if v.len() >= 20 => {
            let mut b: [u8; 16] = v[4..20].try_into().ok()?;
            if xor {
                let key: Vec<u8> = magic.iter().chain(tid.iter()).copied().collect();
                for (x, k) in b.iter_mut().zip(key) {
                    *x ^= k;
                }
            }
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(b)), port))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let nonce = 0x0102_0304_0506_0708;
        let req = request(42, nonce);
        assert_eq!(&req[4..8], &MAGIC.to_be_bytes());
        // Build a success response with XOR-MAPPED-ADDRESS 203.0.113.5:50000.
        let mut resp = req.to_vec();
        resp[0..2].copy_from_slice(&BINDING_SUCCESS.to_be_bytes());
        resp[2..4].copy_from_slice(&12u16.to_be_bytes());
        let port = 50000u16 ^ (MAGIC >> 16) as u16;
        let ip = [203u8, 0, 113, 5];
        let m = MAGIC.to_be_bytes();
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&8u16.to_be_bytes());
        resp.extend_from_slice(&[0, 1]);
        resp.extend_from_slice(&port.to_be_bytes());
        resp.extend((0..4).map(|i| ip[i] ^ m[i]));
        let r = parse(&resp, nonce).unwrap();
        assert_eq!(r.seq, 42);
        assert_eq!(r.mapped, Some("203.0.113.5:50000".parse().unwrap()));
        assert_eq!(parse(&resp, nonce + 1), None);
        assert_eq!(parse(&req, nonce), None);
    }
}
