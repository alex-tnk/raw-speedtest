use super::histogram::LogHistogram;
use super::ring::Ring;
use crate::model::Secs;

/// Aggregate of all samples whose time falls in `[t0, t0 + width)`.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub t0: Secs,
    pub n: u32,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub abs_delta_sum: f64,
    pub deltas: u32,
    /// Samples without a value (lost probe, failed ping).
    pub misses: u32,
}

impl Bucket {
    fn new(t0: Secs) -> Bucket {
        Bucket {
            t0,
            n: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            abs_delta_sum: 0.0,
            deltas: 0,
            misses: 0,
        }
    }

    pub fn mean(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / self.n as f64)
    }

    pub fn jitter(&self) -> Option<f64> {
        (self.deltas > 0).then(|| self.abs_delta_sum / self.deltas as f64)
    }
}

/// One downsampling level: completed buckets in a ring plus the open one.
#[derive(Clone, Debug)]
pub struct Tier {
    pub width: Secs,
    current: Option<Bucket>,
    done: Ring<Bucket>,
}

impl Tier {
    pub fn new(width: Secs, cap: usize) -> Tier {
        Tier {
            width,
            current: None,
            done: Ring::new(cap),
        }
    }

    fn add(&mut self, t: Secs, v: Option<f64>, delta: Option<f64>) {
        let t0 = (t / self.width).floor() * self.width;
        match &self.current {
            Some(b) if b.t0 == t0 => {}
            _ => {
                if let Some(b) = self.current.take() {
                    self.done.push(b);
                }
                self.current = Some(Bucket::new(t0));
            }
        }
        let b = self.current.as_mut().expect("bucket just ensured");
        match v {
            Some(v) => {
                b.n += 1;
                b.sum += v;
                b.min = b.min.min(v);
                b.max = b.max.max(v);
            }
            None => b.misses += 1,
        }
        if let Some(d) = delta {
            b.abs_delta_sum += d.abs();
            b.deltas += 1;
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.done.len() + usize::from(self.current.is_some())
    }

    /// All buckets, oldest first, including the open one.
    pub fn iter(&self) -> impl Iterator<Item = &Bucket> + Clone {
        self.done.iter().chain(self.current.iter())
    }

    /// Index of the first bucket that ends after `t`.
    pub fn first_ending_after(&self, t: Secs) -> usize {
        self.done.partition_point(|b| b.t0 + self.width <= t)
    }

    pub fn get(&self, i: usize) -> Option<&Bucket> {
        if i < self.done.len() {
            self.done.get(i)
        } else if i == self.done.len() {
            self.current.as_ref()
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.done.clear();
    }
}

/// A sample stored in a `Series`.
pub trait Sample {
    fn t(&self) -> Secs;
    /// `None` for a miss (lost probe, failed ping).
    fn value(&self) -> Option<f64>;
}

/// Raw samples in a bounded ring. Every sample is also folded into minute
/// and hour aggregates and an all-run histogram at ingestion, so once raw
/// samples age out of the ring the long-range view and percentiles remain.
#[derive(Clone, Debug)]
pub struct Series<T> {
    pub raw: Ring<T>,
    pub minute: Tier,
    pub hour: Tier,
    pub hist: LogHistogram,
    last_value: Option<f64>,
    count: u64,
}

impl<T: Sample> Series<T> {
    /// `raw_cap` raw samples, 30 days of minutes, a year of hours.
    pub fn new(raw_cap: usize) -> Series<T> {
        Series {
            raw: Ring::new(raw_cap),
            minute: Tier::new(60.0, 30 * 24 * 60),
            hour: Tier::new(3600.0, 366 * 24),
            hist: LogHistogram::new(),
            last_value: None,
            count: 0,
        }
    }

    pub fn push(&mut self, s: T) {
        let t = s.t();
        let v = s.value();
        let delta = match (v, self.last_value) {
            (Some(v), Some(p)) => Some(v - p),
            _ => None,
        };
        self.minute.add(t, v, delta);
        self.hour.add(t, v, delta);
        if let Some(v) = v {
            self.hist.add(v);
            self.last_value = Some(v);
            self.count += 1;
        }
        self.raw.push(s);
    }

    /// Time of the oldest raw sample still held.
    pub fn raw_start(&self) -> Option<Secs> {
        self.raw.get(0).map(Sample::t)
    }

    /// True if the raw ring still holds every sample ever pushed.
    pub fn raw_complete(&self) -> bool {
        self.raw.evicted() == 0
    }

    /// Samples with a value, all run.
    #[cfg(test)]
    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn clear(&mut self) {
        self.raw.clear();
        self.minute.clear();
        self.hour.clear();
        self.hist.clear();
        self.last_value = None;
        self.count = 0;
    }

    /// Raw index range of samples with `from <= t < to`.
    pub fn raw_range(&self, from: Secs, to: Secs) -> (usize, usize) {
        let a = self.raw.partition_point(|s| s.t() < from);
        let b = self.raw.partition_point(|s| s.t() < to);
        (a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct S(f64, Option<f64>);
    impl Sample for S {
        fn t(&self) -> Secs {
            self.0
        }
        fn value(&self) -> Option<f64> {
            self.1
        }
    }

    #[test]
    fn buckets_aggregate() {
        let mut s: Series<S> = Series::new(4);
        s.push(S(1.0, Some(10.0)));
        s.push(S(2.0, Some(14.0)));
        s.push(S(59.0, None));
        s.push(S(61.0, Some(12.0)));
        s.push(S(62.0, Some(12.0)));
        let b: Vec<_> = s.minute.iter().copied().collect();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].n, 2);
        assert_eq!(b[0].misses, 1);
        assert_eq!(b[0].mean(), Some(12.0));
        assert_eq!(b[0].jitter(), Some(4.0));
        assert_eq!(b[1].min, 12.0);
        // Raw ring evicted the first sample but the aggregates kept it.
        assert_eq!(s.raw.len(), 4);
        assert!(!s.raw_complete());
        assert_eq!(s.count(), 4);
        assert_eq!(s.raw_range(2.0, 62.0), (0, 3));
    }

    #[test]
    fn long_run_is_bounded() {
        // Ten simulated days of one sample per second.
        let mut s: Series<S> = Series::new(100_000);
        for i in 0..864_000u32 {
            s.push(S(f64::from(i), Some(f64::from(i % 50))));
        }
        assert_eq!(s.raw.len(), 100_000);
        assert_eq!(s.minute.len(), 14_400);
        assert_eq!(s.hour.len(), 240);
        assert_eq!(s.count(), 864_000);
    }
}
