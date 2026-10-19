use std::time::{Duration, Instant};

use super::RunCtx;
use crate::model::{Direction, Event, ThroughputSample};

/// Length of one fine-grained throughput bin.
pub const BIN: Duration = Duration::from_millis(50);

/// Turns a stream of `(time, cumulative bytes)` observations into
/// fixed-length throughput bins, one event per bin.
pub struct Binner<'a> {
    ctx: &'a RunCtx,
    dir: Direction,
    request: u32,
    /// `None` until the first observation when the bins start at the
    /// first byte rather than at a known instant.
    bin_start: Option<Instant>,
    bytes_at_start: u64,
    last_total: u64,
    bins: u32,
    /// Last closed bin, emitted when the next one closes so that a short
    /// final bin can be merged into it instead of showing a burst.
    held: Option<ThroughputSample>,
}

impl<'a> Binner<'a> {
    /// Bins start at `start`, or at the first observation if `None`.
    pub fn new(
        ctx: &'a RunCtx,
        dir: Direction,
        request: u32,
        start: Option<Instant>,
    ) -> Binner<'a> {
        Binner {
            ctx,
            dir,
            request,
            bin_start: start,
            bytes_at_start: 0,
            last_total: 0,
            bins: 0,
            held: None,
        }
    }

    pub fn add(&mut self, t: Instant, total: u64) {
        if self.bin_start.is_none() {
            // Bytes delivered by the read that starts the first bin arrived
            // during the wait for the first byte, not inside the bin.
            self.bytes_at_start = total;
        }
        self.last_total = self.last_total.max(total);
        let start = *self.bin_start.get_or_insert(t);
        if t.saturating_duration_since(start) >= BIN {
            self.close(t);
        }
    }

    fn close(&mut self, t: Instant) {
        let Some(start) = self.bin_start else { return };
        let dt = t.saturating_duration_since(start).as_secs_f64();
        if dt <= 0.0 {
            return;
        }
        let bytes = self.last_total.saturating_sub(self.bytes_at_start);
        let sample = ThroughputSample {
            t: self.ctx.at(t),
            dt: dt as f32,
            bps: bytes as f64 * 8.0 / dt,
            dir: self.dir,
            request: self.request,
            bin: self.bins,
        };
        if let Some(prev) = self.held.replace(sample) {
            self.ctx.emit(Event::Throughput(prev));
        }
        self.bins += 1;
        self.bin_start = Some(t);
        self.bytes_at_start = self.last_total;
    }

    /// Close the last bin at `t` with `total` bytes. A tail shorter than
    /// half a bin is merged into the previous bin.
    pub fn finish(mut self, t: Instant, total: u64) {
        self.last_total = self.last_total.max(total);
        let tail_bytes = self.last_total.saturating_sub(self.bytes_at_start);
        let tail_dt = self
            .bin_start
            .map_or(0.0, |s| t.saturating_duration_since(s).as_secs_f64());
        match self.held.take() {
            Some(mut prev) if tail_dt < BIN.as_secs_f64() / 2.0 => {
                let prev_dt = f64::from(prev.dt);
                let prev_bytes = prev.bps * prev_dt / 8.0;
                let dt = prev_dt + tail_dt;
                prev.bps = (prev_bytes + tail_bytes as f64) * 8.0 / dt;
                prev.dt = dt as f32;
                prev.t = self.ctx.at(t);
                self.ctx.emit(Event::Throughput(prev));
            }
            held => {
                if let Some(prev) = held {
                    self.ctx.emit(Event::Throughput(prev));
                }
                if tail_bytes > 0 && tail_dt > 0.0 {
                    self.close(t);
                    if let Some(last) = self.held.take() {
                        self.ctx.emit(Event::Throughput(last));
                    }
                }
            }
        }
    }
}
