/// Log-scale histogram with 1 % wide bins from 1e-3 to 1e12. Used for
/// whole-run percentiles once raw samples have aged out of their ring; the
/// reported percentile is within 0.5 % of the true sample value.
#[derive(Clone, Debug)]
pub struct LogHistogram {
    counts: Vec<u32>,
    zero_or_less: u64,
    n: u64,
    sum: f64,
    min: f64,
    max: f64,
}

const MIN: f64 = 1e-3;
const MAX: f64 = 1e12;
const RATIO: f64 = 1.01;

fn bins() -> usize {
    ((MAX / MIN).ln() / RATIO.ln()).ceil() as usize + 1
}

impl Default for LogHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LogHistogram {
    pub fn new() -> LogHistogram {
        LogHistogram {
            counts: Vec::new(),
            zero_or_less: 0,
            n: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    pub fn add(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        self.n += 1;
        self.sum += v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        if v <= 0.0 {
            self.zero_or_less += 1;
            return;
        }
        if self.counts.is_empty() {
            // Allocated lazily so empty series cost nothing.
            self.counts = vec![0; bins()];
        }
        let i = ((v.max(MIN) / MIN).ln() / RATIO.ln()) as usize;
        let i = i.min(self.counts.len() - 1);
        self.counts[i] = self.counts[i].saturating_add(1);
    }

    pub fn len(&self) -> u64 {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn min(&self) -> Option<f64> {
        (self.n > 0).then_some(self.min)
    }

    pub fn mean(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / self.n as f64)
    }

    pub fn max(&self) -> Option<f64> {
        (self.n > 0).then_some(self.max)
    }

    /// Value at quantile `p` (0..=1), clamped to the observed min and max.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        let rank = (p.clamp(0.0, 1.0) * (self.n - 1) as f64).round() as u64;
        if rank == 0 {
            return Some(self.min);
        }
        if rank == self.n - 1 {
            return Some(self.max);
        }
        if rank < self.zero_or_less {
            return Some(self.min.min(0.0));
        }
        let mut seen = self.zero_or_less;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += u64::from(c);
            if seen > rank {
                let lo = MIN * RATIO.powi(i as i32);
                let mid = lo * RATIO.sqrt();
                return Some(mid.clamp(self.min, self.max));
            }
        }
        Some(self.max)
    }

    pub fn clear(&mut self) {
        *self = LogHistogram::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_close() {
        let mut h = LogHistogram::new();
        for i in 1..=1000 {
            h.add(f64::from(i));
        }
        let p50 = h.percentile(0.5).unwrap();
        assert!((p50 - 500.0).abs() / 500.0 < 0.01, "{p50}");
        let p99 = h.percentile(0.99).unwrap();
        assert!((p99 - 990.0).abs() / 990.0 < 0.01, "{p99}");
        assert_eq!(h.percentile(1.0), Some(1000.0));
        assert_eq!(h.percentile(0.0), Some(1.0));
    }
}
