/// Order statistics of a set of samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

impl Summary {
    /// Every value multiplied by `k` (unit conversion).
    pub fn scaled(&self, k: f64) -> Summary {
        Summary {
            n: self.n,
            min: self.min * k,
            p25: self.p25 * k,
            median: self.median * k,
            p75: self.p75 * k,
            p90: self.p90 * k,
            p99: self.p99 * k,
            max: self.max * k,
            mean: self.mean * k,
        }
    }
}

/// Percentile of sorted values with linear interpolation between the two
/// nearest ranks (the method Cloudflare's speed test uses).
pub fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let idx = (sorted.len() - 1) as f64 * p.clamp(0.0, 1.0);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    let rem = idx - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * rem
}

/// Summarize `values`, sorting the scratch buffer in place. Non-finite
/// values are ignored.
pub fn summarize(scratch: &mut Vec<f64>) -> Option<Summary> {
    scratch.retain(|v| v.is_finite());
    if scratch.is_empty() {
        return None;
    }
    scratch.sort_unstable_by(f64::total_cmp);
    let s = &scratch[..];
    let sum: f64 = s.iter().sum();
    Some(Summary {
        n: s.len(),
        min: s[0],
        p25: percentile_sorted(s, 0.25),
        median: percentile_sorted(s, 0.5),
        p75: percentile_sorted(s, 0.75),
        p90: percentile_sorted(s, 0.9),
        p99: percentile_sorted(s, 0.99),
        max: s[s.len() - 1],
        mean: sum / s.len() as f64,
    })
}

/// Summarize an iterator of values using a reusable scratch buffer.
pub fn summarize_iter(
    scratch: &mut Vec<f64>,
    values: impl Iterator<Item = f64>,
) -> Option<Summary> {
    scratch.clear();
    scratch.extend(values);
    summarize(scratch)
}

/// Mean absolute difference between consecutive samples, in time order.
/// This is how Cloudflare's speed test defines jitter.
pub fn jitter(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut prev: Option<f64> = None;
    let mut sum = 0.0;
    let mut n = 0usize;
    for v in values {
        if let Some(p) = prev {
            sum += (v - p).abs();
            n += 1;
        }
        prev = Some(v);
    }
    if n == 0 { None } else { Some(sum / n as f64) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_interpolate() {
        let s = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile_sorted(&s, 0.0), 1.0);
        assert_eq!(percentile_sorted(&s, 1.0), 4.0);
        assert!((percentile_sorted(&s, 0.5) - 2.5).abs() < 1e-12);
        assert!((percentile_sorted(&s, 0.9) - 3.7).abs() < 1e-12);
    }

    #[test]
    fn summary_ignores_nan() {
        let mut v = vec![5.0, f64::NAN, 1.0, 3.0];
        let s = summarize(&mut v).unwrap();
        assert_eq!(s.n, 3);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.median, 3.0);
        assert_eq!(s.max, 5.0);
        assert_eq!(s.mean, 3.0);
    }

    #[test]
    fn jitter_is_mean_abs_delta() {
        assert_eq!(
            jitter([10.0, 12.0, 11.0, 15.0].into_iter()),
            Some(7.0 / 3.0)
        );
        assert_eq!(jitter([10.0].into_iter()), None);
    }
}
