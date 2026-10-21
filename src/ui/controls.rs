//! Run controls, status line, progress and continuous-mode settings.

use std::time::Duration;

use eframe::egui::{Align2, Color32, Sense, Ui, pos2, vec2};

use super::text::{self, Text};
use super::widgets::{self, Kind, Tip, tooltip};
use super::{format, theme};
use crate::app::derived::Derived;
use crate::app::session::{RunState, Session};
use crate::model::{Config, IpFamily, Phase, RunMode};

pub enum Action {
    Start,
    Stop,
    Pause,
    Resume,
    Reset,
    Export,
    Mode(RunMode),
    Config(Config),
    ToggleSettings,
    /// Keep the app in the menu bar (and running) when the window closes.
    MenuBar(bool),
}

/// Status line text and color.
pub(super) fn status(s: &Session, notice: Option<(&str, bool)>) -> (String, Color32) {
    let now = s.now().unwrap_or(0.0);
    if let Some((msg, err)) = notice {
        return (msg.to_owned(), if err { theme::ERROR } else { theme::TEXT });
    }
    if s.active()
        && let Some((t, msg)) = s.errors.last()
        && now - t < 10.0
    {
        return (msg.clone(), theme::ERROR);
    }
    match s.state {
        RunState::Idle => ("Idle".into(), theme::TEXT_2),
        RunState::Paused => ("Paused".into(), theme::TEXT),
        RunState::Finished => (
            format!(
                "Finished · {}",
                format::elapsed(s.finished_at.unwrap_or(0.0))
            ),
            theme::TEXT,
        ),
        RunState::Running => match &s.phase {
            None => ("Starting".into(), theme::TEXT),
            Some(Phase::Connecting) => (
                format!("Connecting · {}", crate::net::cloudflare::HOST),
                theme::TEXT,
            ),
            Some(Phase::Latency { done, total }) => (
                format!("Measuring latency · {done}/{total} probes"),
                theme::TEXT,
            ),
            Some(Phase::Sustained { dir, streams, .. }) => (
                format!("{} · {streams} connections", dir.label()),
                theme::TEXT,
            ),
            Some(Phase::PacketLoss { sent, total }) => (
                format!("Measuring packet loss · {sent}/{total} probes"),
                theme::TEXT,
            ),
            Some(Phase::Waiting { next_cycle_at }) => (
                format!(
                    "Next throughput cycle in {}",
                    format::elapsed((next_cycle_at - now).ceil())
                ),
                theme::TEXT,
            ),
            Some(Phase::BackingOff { until, reason }) => (
                format!(
                    "Retrying in {} · {reason}",
                    format::elapsed((until - now).ceil())
                ),
                theme::WARNING,
            ),
        },
    }
}

pub fn show(
    ui: &mut Ui,
    text: &Text,
    s: &Session,
    mode: RunMode,
    notice: Option<(&str, bool)>,
    actions: &mut Vec<Action>,
) {
    let active = s.active();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::S8;
        let sel = match mode {
            RunMode::OneShot => 0,
            RunMode::Continuous => 1,
        };
        if let Some(i) = widgets::segmented(ui, text, &["One-shot", "Continuous"], sel, !active) {
            actions.push(Action::Mode(if i == 0 {
                RunMode::OneShot
            } else {
                RunMode::Continuous
            }));
        }
        if active {
            if widgets::button(ui, text, "Stop", Kind::Primary, true).clicked() {
                actions.push(Action::Stop);
            }
            if s.state == RunState::Paused {
                if widgets::button(ui, text, "Resume", Kind::Secondary, true).clicked() {
                    actions.push(Action::Resume);
                }
            } else if widgets::button(ui, text, "Pause", Kind::Secondary, true).clicked() {
                actions.push(Action::Pause);
            }
        } else {
            if widgets::button(ui, text, "Start", Kind::Primary, true).clicked() {
                actions.push(Action::Start);
            }
            widgets::button(ui, text, "Pause", Kind::Secondary, false);
        }
        if widgets::button(ui, text, "Reset", Kind::Secondary, active || s.has_data()).clicked() {
            actions.push(Action::Reset);
        }
        if widgets::button(ui, text, "Export…", Kind::Secondary, s.has_data()).clicked() {
            actions.push(Action::Export);
        }
        if widgets::button(ui, text, "Settings", Kind::Secondary, true).clicked() {
            actions.push(Action::ToggleSettings);
        }
        // Status, then elapsed time at the right edge.
        let rest = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(rest, theme::BUTTON_HEIGHT), Sense::hover());
        let p = ui.painter();
        let (msg, color) = status(s, notice);
        let elapsed_w = if s.wall_start.is_some() {
            let e = format::elapsed(s.now().unwrap_or(0.0));
            let vr = text.paint(
                p,
                pos2(r.right(), r.center().y),
                Align2::RIGHT_CENTER,
                &e,
                text::BODY,
            );
            let lr = text.paint(
                p,
                pos2(vr.left() - theme::S8, r.center().y),
                Align2::RIGHT_CENTER,
                "Elapsed",
                text::BODY_2,
            );
            r.right() - lr.left() + theme::S16
        } else {
            0.0
        };
        let room = r.width() - elapsed_w - theme::S8;
        let msg = text.elide_end(p, &msg, text::BODY, room);
        text.paint(
            p,
            pos2(r.left() + theme::S8, r.center().y),
            Align2::LEFT_CENTER,
            &msg,
            text::BODY.color(color),
        );
    });
    ui.add_space(theme::S8);
    let frac = match (s.mode, s.state) {
        (Some(RunMode::OneShot), RunState::Running | RunState::Paused) => Some(s.progress),
        (Some(RunMode::OneShot), RunState::Finished) => Some(1.0),
        (Some(RunMode::Continuous), RunState::Running | RunState::Paused) => match &s.phase {
            Some(Phase::Waiting { next_cycle_at }) => {
                let iv = s.config.continuous.cycle_interval.as_secs_f64().max(1.0);
                let now = s.now().unwrap_or(0.0);
                Some((1.0 - ((next_cycle_at - now) / iv).clamp(0.0, 1.0)) as f32)
            }
            _ => None,
        },
        _ => None,
    };
    widgets::progress(ui, frac);
}

const INTERVALS: [(u64, &str); 7] = [
    (60, "1 min"),
    (120, "2 min"),
    (300, "5 min"),
    (600, "10 min"),
    (900, "15 min"),
    (1800, "30 min"),
    (3600, "60 min"),
];
const LATENCY_MS: [(u64, &str); 5] = [
    (500, "0.5 s"),
    (1000, "1 s"),
    (2000, "2 s"),
    (5000, "5 s"),
    (10_000, "10 s"),
];
const LOSS_HZ: [(f64, &str); 6] = [
    (0.0, "Off"),
    (1.0, "1/s"),
    (2.0, "2/s"),
    (5.0, "5/s"),
    (10.0, "10/s"),
    (20.0, "20/s"),
];
const FAMILIES: [IpFamily; 3] = [IpFamily::Auto, IpFamily::V4, IpFamily::V6];

/// Continuous-mode load, IP version and, where supported, the menu bar
/// switch. Returns a changed config and a changed menu bar setting.
pub fn settings(
    ui: &mut Ui,
    text: &Text,
    cfg: &Config,
    d: &Derived,
    menu_bar: Option<bool>,
) -> (Option<Config>, Option<bool>) {
    let mut menu_bar_change = None;
    let mut out = *cfg;
    let mut changed = false;
    let mut family = out.family;
    let c = &mut out.continuous;
    widgets::frame().show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(theme::S8, theme::S8);
        let iv = INTERVALS
            .iter()
            .position(|(s, _)| Duration::from_secs(*s) == c.cycle_interval)
            .unwrap_or(4);
        if let Some(i) = widgets::option_row(ui, text, "Throughput every", &INTERVALS.map(|x| x.1), iv) {
            c.cycle_interval = Duration::from_secs(INTERVALS[i].0);
            changed = true;
        }
        let li = LATENCY_MS
            .iter()
            .position(|(ms, _)| Duration::from_millis(*ms) == c.latency_interval)
            .unwrap_or(1);
        if let Some(i) = widgets::option_row(ui, text, "Latency probe every", &LATENCY_MS.map(|x| x.1), li) {
            c.latency_interval = Duration::from_millis(LATENCY_MS[i].0);
            changed = true;
        }
        let hi = LOSS_HZ
            .iter()
            .position(|(hz, _)| (*hz - c.loss_rate_hz).abs() < 1e-9)
            .unwrap_or(3);
        if let Some(i) = widgets::option_row(ui, text, "UDP probes", &LOSS_HZ.map(|x| x.1), hi) {
            c.loss_rate_hz = LOSS_HZ[i].0;
            changed = true;
        }
        let fi = FAMILIES.iter().position(|f| *f == family).unwrap_or(0);
        if let Some(i) = widgets::option_row(ui, text, "IP version", &FAMILIES.map(|f| f.label()), fi) {
            family = FAMILIES[i];
            changed = true;
        }
        if let Some(on) = menu_bar
            && let Some(i) = widgets::option_row(ui, text, "Run in menu bar", &["Off", "On"], usize::from(on))
        {
            menu_bar_change = Some(i == 1);
        }
        // Throughput data per hour at these settings.
        let per_cycle = match (d.download.value, d.upload.value) {
            (Some(a), Some(b)) => Some((a + b) / 8.0 * c.sustain.as_secs_f64()),
            _ => None,
        };
        let per_hour = per_cycle.map(|b| b * 3600.0 / c.cycle_interval.as_secs_f64());
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), theme::TABLE_ROW), Sense::hover());
        let p = ui.painter();
        text.paint(p, pos2(r.left(), r.center().y), Align2::LEFT_CENTER, "Throughput data per hour", text::BODY_2);
        let v = match per_hour {
            Some(b) if b >= 1e9 => format!("≈ {} GB", format::sig3(b / 1e9)),
            Some(b) => format!("≈ {} MB", format::sig3(b / 1e6)),
            None => "—".into(),
        };
        let lw = text.width(p, "Throughput data per hour", text::BODY_2);
        let vr = text.paint(p, pos2(r.left() + lw + theme::S12, r.center().y), Align2::LEFT_CENTER, &v, text::BODY);
        if let Some(hp) = resp.hover_pos().filter(|hp| hp.x <= vr.right() + theme::S8) {
            tooltip(
                ui,
                text,
                hp,
                &Tip::default().note(
                    "Payload × requests × 2 directions × cycles per hour. Auto payloads are sized for one second at the last measured rate.",
                ),
            );
        }
    });
    out.family = family;
    (changed.then_some(out), menu_bar_change)
}
