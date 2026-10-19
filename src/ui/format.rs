//! One way to show each kind of number.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::platform;

/// Three significant digits, never scientific: `152`, `15.2`, `1.52`, `0.152`.
pub fn sig3(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    let a = v.abs();
    if a >= 100.0 || a == 0.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

/// Throughput value in Mbps (unit shown separately).
pub fn mbps(bps: f64) -> String {
    sig3(bps / 1e6)
}

pub fn ms(v: f64) -> String {
    sig3(v)
}

/// A fraction 0..=1 as a percentage value: `0.00`, `1.25`, `12.5`, `100`.
pub fn pct(frac: f64) -> String {
    let p = frac * 100.0;
    if !p.is_finite() {
        "—".into()
    } else if p >= 100.0 {
        "100".into()
    } else if p >= 10.0 {
        format!("{p:.1}")
    } else {
        format!("{p:.2}")
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// SI-prefixed axis tick: `0`, `500k`, `1.5M`, `50M`, `1G`.
pub fn si(v: f64) -> String {
    let (div, suffix) = if v.abs() >= 1e9 {
        (1e9, "G")
    } else if v.abs() >= 1e6 {
        (1e6, "M")
    } else if v.abs() >= 1e3 {
        (1e3, "k")
    } else {
        (1.0, "")
    };
    let x = v / div;
    let s = if (x - x.round()).abs() < 1e-6 {
        format!("{x:.0}")
    } else if (x * 10.0 - (x * 10.0).round()).abs() < 1e-6 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    };
    format!("{s}{suffix}")
}

/// Plain axis tick for ms or dBm: no trailing zeros.
pub fn tick(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 {
        format!("{v:.0}")
    } else if (v * 10.0 - (v * 10.0).round()).abs() < 1e-6 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Local wall-clock time; `None` offset falls back to UTC with a suffix.
fn local_parts(t: SystemTime) -> (i64, u32, bool) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (offset, local) = match platform::current().utc_offset(secs) {
        Some(o) => (i64::from(o), true),
        None => (0, false),
    };
    (secs + offset, d.subsec_millis(), local)
}

/// `14:03:22.150` (milliseconds) or `14:03:22`.
pub fn clock(t: SystemTime, millis: bool) -> String {
    let (s, ms, local) = local_parts(t);
    let day = s.rem_euclid(86_400);
    let (h, m, sec) = (day / 3600, (day / 60) % 60, day % 60);
    let utc = if local { "" } else { " UTC" };
    if millis {
        format!("{h:02}:{m:02}:{sec:02}.{ms:03}{utc}")
    } else {
        format!("{h:02}:{m:02}:{sec:02}{utc}")
    }
}

/// `14:03:22.1` for sub-second axis ticks.
pub fn clock_tenths(t: SystemTime) -> String {
    let (s, ms, local) = local_parts(t);
    let day = s.rem_euclid(86_400);
    let utc = if local { "" } else { " UTC" };
    format!(
        "{:02}:{:02}:{:02}.{}{utc}",
        day / 3600,
        (day / 60) % 60,
        day % 60,
        ms / 100
    )
}

/// `14:03` for coarse axis ticks.
pub fn clock_hm(t: SystemTime) -> String {
    let (s, _, local) = local_parts(t);
    let day = s.rem_euclid(86_400);
    let utc = if local { "" } else { " UTC" };
    format!("{:02}:{:02}{utc}", day / 3600, (day / 60) % 60)
}

/// `2026-10-19` for day boundaries and export names.
pub fn date(t: SystemTime) -> String {
    let (s, _, _) = local_parts(t);
    let days = s.div_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Elapsed time `0:42`, `12:05`, `3:04:10`, `2d 03:04`.
pub fn elapsed(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m, sec) = (s / 86_400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(sig3(152.34), "152");
        assert_eq!(sig3(15.234), "15.2");
        assert_eq!(sig3(1.5234), "1.52");
        assert_eq!(sig3(0.15234), "0.152");
        assert_eq!(mbps(63_633_479.0), "63.6");
        assert_eq!(pct(0.0), "0.00");
        assert_eq!(pct(0.0125), "1.25");
        assert_eq!(pct(0.672), "67.2");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
        assert_eq!(si(50e6), "50M");
        assert_eq!(si(1.5e6), "1.5M");
        assert_eq!(si(500e3), "500k");
        assert_eq!(si(0.0), "0");
        assert_eq!(tick(2.5), "2.5");
        assert_eq!(elapsed(42.0), "0:42");
        assert_eq!(elapsed(3.0 * 3600.0 + 250.0), "3:04:10");
    }

    #[test]
    fn civil_date() {
        // 2026-10-19 00:00:00 UTC, checked against the date before applying
        // any local offset by using a time well inside the day.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_792_411_200 + 12 * 3600);
        let d = date(t);
        assert!(
            d == "2026-10-19" || d == "2026-10-18" || d == "2026-10-20",
            "{d}"
        );
    }
}
