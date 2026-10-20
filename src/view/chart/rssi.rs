//! Wi-Fi RSSI over the run, a sparkline in the connection info column.

use super::{Decimator, Hover, segments};
use crate::app::session::Session;
use crate::view::format;
use crate::view::palette::{self, Color};
use crate::view::scene::{HAlign, Point, Rect, Scene, TextSize, VAlign, pt};
use crate::view::tip::Tip;

/// Height of the sparkline.
pub const HEIGHT: f32 = palette::S16 * 3.0;

/// Whether there is enough to draw.
pub fn shown(s: &Session) -> bool {
    s.wifi_samples.raw.len() >= 2
}

struct Geometry {
    plot: Rect,
    t0: f64,
    t1: f64,
    lo: f64,
    hi: f64,
}

impl Geometry {
    fn new(s: &Session, w: f32, h: f32) -> Option<Geometry> {
        let raw = &s.wifi_samples.raw;
        if raw.len() < 2 {
            return None;
        }
        let (lo, hi) = (s.wifi_samples.hist.min()?, s.wifi_samples.hist.max()?);
        let (lo, hi) = ((lo - 2.0).floor(), (hi + 2.0).ceil());
        let t0 = raw.get(0).map_or(0.0, |w| w.t);
        let t1 = raw.last().map_or(1.0, |w| w.t).max(t0 + 1.0);
        Some(Geometry {
            plot: Rect {
                x0: palette::SPARK_AXIS,
                y0: palette::S8,
                x1: w.max(palette::SPARK_AXIS + 1.0),
                y1: h.max(palette::S8 + 1.0),
            },
            t0,
            t1,
            lo,
            hi,
        })
    }
    fn x(&self, t: f64) -> f32 {
        self.plot.x0 + ((t - self.t0) / (self.t1 - self.t0)) as f32 * self.plot.width()
    }
    fn y(&self, v: f64) -> f32 {
        self.plot.y1 - ((v - self.lo) / (self.hi - self.lo)) as f32 * self.plot.height()
    }
}

pub fn scene(
    s: &Session,
    w: f32,
    h: f32,
    sc: &mut Scene,
    pts: &mut Vec<Point>,
    breaks: &mut Vec<usize>,
) {
    let Some(g) = Geometry::new(s, w, h) else {
        sc.clear(Rect::default());
        return;
    };
    let plot = g.plot;
    sc.clear(plot);
    sc.segment(
        pt(plot.x0, plot.y1 + 0.5),
        pt(plot.x1, plot.y1 + 0.5),
        palette::STROKE,
        Color::Grid,
        1.0,
    );
    sc.text(
        pt(plot.x0 - palette::S4, g.y(g.hi)),
        &format!("{:.0}", g.hi),
        TextSize::Small,
        Color::Text2,
        HAlign::End,
        VAlign::Top,
    );
    sc.text(
        pt(plot.x0 - palette::S4, g.y(g.lo)),
        &format!("{:.0}", g.lo),
        TextSize::Small,
        Color::Text2,
        HAlign::End,
        VAlign::Bottom,
    );
    let mut dec = Decimator::new(pts, breaks);
    for w in s.wifi_samples.raw.iter() {
        match w.rssi_dbm {
            Some(r) => dec.push(pt(g.x(w.t), g.y(f64::from(r)))),
            None => dec.gap(),
        }
    }
    dec.finish();
    for (i, j) in segments(breaks) {
        sc.line(&pts[i..j], palette::DATA_LINE, Color::Text, 1.0);
    }
    sc.text(
        pt(plot.x1, 0.0),
        "RSSI dBm",
        TextSize::Small,
        Color::Text2,
        HAlign::End,
        VAlign::Top,
    );
}

pub fn hover(s: &Session, w: f32, h: f32, hp: Point) -> Hover {
    let Some(g) = Geometry::new(s, w, h) else {
        return Hover::default();
    };
    if !g.plot.contains(hp) {
        return Hover::default();
    }
    let raw = &s.wifi_samples.raw;
    let t = g.t0 + f64::from((hp.x - g.plot.x0) / g.plot.width()) * (g.t1 - g.t0);
    // The nearest sample in time.
    let i = raw.partition_point(|w| w.t < t);
    let Some(w) = [i.saturating_sub(1), i.min(raw.len() - 1)]
        .into_iter()
        .filter_map(|j| raw.get(j))
        .min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()))
    else {
        return Hover::default();
    };
    let Some(r) = w.rssi_dbm else {
        return Hover::default();
    };
    let p = pt(g.x(w.t), g.y(f64::from(r)));
    let mut tip = Tip::default()
        .title("Wi-Fi sample")
        .pair("RSSI", format!("{r} dBm"));
    if let Some(n) = w.noise_dbm {
        tip.push_pair("Noise", format!("{n} dBm"));
    }
    if let Some(tx) = w.tx_rate_mbps {
        tip.push_pair("Tx rate", format!("{} Mbps", format::sig3(tx)));
    }
    if let Some(wall) = s.wall_at(w.t) {
        tip.push_pair("Time", format::clock(wall, true));
    }
    Hover {
        crosshair: None,
        highlight: Some((p, Color::Text)),
        tip: Some(tip),
        anchor: p,
    }
}
