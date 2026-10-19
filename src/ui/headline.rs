//! The headline values row: download, upload, latency, jitter, loss.

use eframe::egui::{Color32, Id, Rect, Sense, Ui, pos2, vec2};

use super::text::{self, Style, Text};
use super::widgets::{Tip, tooltip};
use super::{format, theme};
use crate::app::derived::{Derived, Headline};

struct Metric<'a> {
    label: &'a str,
    unit: &'a str,
    color: Color32,
    hero: bool,
    value: Option<String>,
    tip: Tip,
}

fn stats_tip(h: &Headline, unit: &str, fmt: &dyn Fn(f64) -> String, how: &str) -> Tip {
    let mut t = Tip::default();
    if let Some(s) = h.summary {
        let v = |x: f64| format!("{} {unit}", fmt(x));
        t.push_pair("Min", v(s.min));
        t.push_pair("Median", v(s.median));
        t.push_pair("p90", v(s.p90));
        t.push_pair("p99", v(s.p99));
        t.push_pair("Max", v(s.max));
        t.push_pair("Samples", format::count(s.n as u64));
    }
    t.note(format!("{}. {how}", h.basis))
}

pub fn height(text: &Text, ui: &Ui) -> f32 {
    let p = ui.painter();
    text.row_height(p, text::BODY)
        + theme::S4
        + text.row_height(p, Style::new(theme::HERO, theme::TEXT))
}

pub fn show(ui: &mut Ui, text: &Text, d: &Derived) {
    let h = height(text, ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let lc = d.loss_basis;
    let mut loss_tip = Tip::default()
        .pair("Sent", format::count(lc.sent()))
        .pair("Lost", format::count(lc.lost));
    if lc.late > 0 {
        loss_tip.push_pair("Late answers", format::count(lc.late));
    }
    if lc.duplicate > 0 {
        loss_tip.push_pair("Duplicates", format::count(lc.duplicate));
    }
    let loss_tip = loss_tip.note(format!(
        "{}. STUN Binding requests to stun.cloudflare.com:3478; each is answered once, so a missing answer is a packet lost in either direction.",
        d.loss.basis
    ));
    let metrics = [
        Metric {
            label: "Download",
            unit: "Mbps",
            color: theme::DOWNLOAD,
            hero: true,
            value: d.download.value.map(format::mbps),
            tip: stats_tip(
                &d.download,
                "Mbps",
                &format::mbps,
                "Per request: body bits ÷ (TTFB − server time + transfer time), as Cloudflare computes it.",
            ),
        },
        Metric {
            label: "Upload",
            unit: "Mbps",
            color: theme::UPLOAD,
            hero: true,
            value: d.upload.value.map(format::mbps),
            tip: stats_tip(
                &d.upload,
                "Mbps",
                &format::mbps,
                "Per request: body bits ÷ time from request start to the first response byte.",
            ),
        },
        Metric {
            label: "Latency",
            unit: "ms",
            color: theme::LATENCY,
            hero: false,
            value: d.latency.value.map(format::ms),
            tip: stats_tip(
                &d.latency,
                "ms",
                &format::ms,
                "Each probe: TTFB of GET __down?bytes=0 minus the Server-Timing server time.",
            ),
        },
        Metric {
            label: "Jitter",
            unit: "ms",
            color: theme::JITTER,
            hero: false,
            value: d.jitter.value.map(format::ms),
            tip: stats_tip(
                &d.jitter,
                "ms",
                &format::ms,
                "Stats are of the absolute differences.",
            ),
        },
        Metric {
            label: "Packet loss",
            unit: "%",
            color: theme::LOSS,
            hero: false,
            value: d.loss.value.map(format::pct),
            tip: loss_tip,
        },
    ];
    let weights = [1.35, 1.35, 1.0, 1.0, 1.0];
    let total: f32 = weights.iter().sum();
    let label_h = text.row_height(ui.painter(), text::BODY);
    let hero_top = rect.top() + label_h + theme::S4;
    let hero = text.laid(ui.painter(), "0", Style::new(theme::HERO, theme::TEXT));
    let baseline = hero_top + hero.baseline();
    let mut x = rect.left();
    for (m, w) in metrics.iter().zip(weights) {
        let width = rect.width() * w / total;
        let block =
            Rect::from_min_size(pos2(x, rect.top()), vec2(width - theme::S16, rect.height()));
        let p = ui.painter();
        text.paint(
            p,
            block.left_top(),
            eframe::egui::Align2::LEFT_TOP,
            m.label,
            text::BODY,
        );
        let (vsize, usize_) = if m.hero {
            (theme::HERO, theme::HERO_UNIT)
        } else {
            (theme::SECONDARY, theme::SECONDARY_UNIT)
        };
        let (value, vcolor) = match &m.value {
            Some(v) => (v.as_str(), m.color),
            None => ("—", theme::TEXT_2),
        };
        let vstyle = Style::new(vsize, vcolor);
        let laid = text.laid(p, value, vstyle);
        let top = baseline - laid.baseline();
        text.value_unit(
            p,
            pos2(block.left(), top),
            (value, vstyle),
            (
                if m.value.is_some() { m.unit } else { "" },
                Style::new(usize_, theme::TEXT_2),
            ),
        );
        let resp = ui.interact(block, Id::unique(("headline", m.label)), Sense::hover());
        if let Some(hp) = resp.hover_pos()
            && m.value.is_some()
        {
            tooltip(ui, text, hp, &m.tip);
        }
        x += width;
    }
}
