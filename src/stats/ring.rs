/// Fixed-capacity FIFO. Pushing into a full ring evicts the oldest item, so
/// memory never grows past `capacity` items.
#[derive(Clone, Debug)]
pub struct Ring<T> {
    buf: Vec<T>,
    cap: usize,
    /// Index of the oldest item once the ring has wrapped.
    head: usize,
    evicted: u64,
}

impl<T> Ring<T> {
    pub fn new(cap: usize) -> Ring<T> {
        assert!(cap > 0);
        Ring {
            buf: Vec::new(),
            cap,
            head: 0,
            evicted: 0,
        }
    }

    pub fn push(&mut self, item: T) -> Option<T> {
        if self.buf.len() < self.cap {
            self.buf.push(item);
            None
        } else {
            let old = std::mem::replace(&mut self.buf[self.head], item);
            self.head = (self.head + 1) % self.cap;
            self.evicted += 1;
            Some(old)
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Items dropped because the ring was full.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
        self.evicted = 0;
    }

    /// Oldest first.
    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.buf.len() {
            return None;
        }
        Some(&self.buf[(self.head + i) % self.buf.len()])
    }

    pub fn last(&self) -> Option<&T> {
        if self.buf.is_empty() {
            None
        } else {
            self.get(self.buf.len() - 1)
        }
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        if self.buf.is_empty() {
            return None;
        }
        let i = (self.head + self.buf.len() - 1) % self.buf.len();
        Some(&mut self.buf[i])
    }

    /// The contents as two slices, oldest first.
    pub fn as_slices(&self) -> (&[T], &[T]) {
        let (a, b) = self.buf.split_at(self.head);
        (b, a)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + Clone {
        let (a, b) = self.as_slices();
        a.iter().chain(b.iter())
    }

    /// First index whose item does not satisfy `pred`, assuming the ring is
    /// partitioned by `pred` (true items first), e.g. sorted by time.
    pub fn partition_point(&self, pred: impl Fn(&T) -> bool) -> usize {
        let (a, b) = self.as_slices();
        let pa = a.partition_point(&pred);
        if pa < a.len() {
            pa
        } else {
            a.len() + b.partition_point(&pred)
        }
    }

    /// Iterate items `[from, to)` by logical index.
    pub fn range(&self, from: usize, to: usize) -> impl Iterator<Item = &T> {
        let to = to.min(self.len());
        let from = from.min(to);
        (from..to).map(move |i| &self.buf[(self.head + i) % self.buf.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_keeps_order() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.evicted(), 2);
        assert_eq!(r.get(0), Some(&2));
        assert_eq!(r.last(), Some(&4));
        assert_eq!(r.partition_point(|&x| x < 4), 2);
        assert_eq!(r.range(1, 3).copied().collect::<Vec<_>>(), vec![3, 4]);
    }

    #[test]
    fn memory_is_bounded() {
        let mut r = Ring::new(1000);
        for i in 0..100_000u32 {
            r.push(i);
        }
        assert_eq!(r.len(), 1000);
        assert_eq!(r.get(0), Some(&99_000));
    }
}
