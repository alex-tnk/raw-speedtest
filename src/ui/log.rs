//! Every request, connection and error, newest first.

use eframe::egui::{Align2, CornerRadius, Rect, ScrollArea, Sense, Ui, pos2, vec2};

use super::text::{self, Style, Text};
use super::widgets::{self, Tip, tooltip};
use super::{format, theme};
use crate::app::session::Session;
use crate::engine::size_label;
use crate::model::Direction;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Requests,
    Connections,
    Errors,
}

struct Col {
    title: &'static str,
    unit: &'static str,
    width: f32,
    right: bool,
}

const fn col(title: &'static str, unit: &'static str, width: f32, right: bool) -> Col {
    Col {
        title,
        unit,
        width,
        right,
    }
}

const REQ_COLS: [Col; 11] = [
    col("Time", "", theme::COL_TIME, false),
    col("Test", "", theme::COL_L, false),
    col("Size", "", theme::COL_S, true),
    col("Sample", "", theme::COL_S, true),
    col("Conn", "", theme::COL_L, true),
    col("TTFB", "ms", theme::COL_L, true),
    col("Server", "ms", theme::COL_L, true),
    col("Transfer", "ms", theme::COL_XL, true),
    col("Duration", "ms", theme::COL_XL, true),
    col("Throughput", "Mbps", theme::COL_XXL, true),
    col("Retrans", "", theme::COL_M, true),
];

const CONN_COLS: [Col; 9] = [
    col("Time", "", theme::COL_TIME, false),
    col("Use", "", theme::COL_USE, false),
    col("Remote", "", theme::COL_ADDR, false),
    col("Local", "", theme::COL_ADDR, false),
    col("DNS", "ms", theme::COL_M, true),
    col("TCP", "ms", theme::COL_M, true),
    col("TLS", "ms", theme::COL_M, true),
    col("Version", "", theme::COL_M, false),
    col("Cipher", "", theme::COL_CIPHER, false),
];

fn header(ui: &mut Ui, text: &Text, cols: &[Col]) {
    let (r, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), theme::TABLE_ROW), Sense::hover());
    let p = ui.painter();
    let mut x = r.left();
    for c in cols {
        let label = if c.unit.is_empty() {
            c.title.to_owned()
        } else {
            format!("{} {}", c.title, c.unit)
        };
        let (pos, align) = if c.right {
            (
                pos2(x + c.width - theme::S8, r.center().y),
                Align2::RIGHT_CENTER,
            )
        } else {
            (pos2(x, r.center().y), Align2::LEFT_CENTER)
        };
        text.paint(p, pos, align, &label, text::SMALL_2);
        x += c.width;
    }
    p.hline(
        r.x_range(),
        r.bottom() + 0.5,
        eframe::egui::Stroke::new(theme::STROKE, theme::BORDER),
    );
}

fn cells(ui: &Ui, text: &Text, r: Rect, cols: &[Col], values: &[(String, Style)]) {
    let p = ui.painter_at(r);
    let mut x = r.left();
    for (c, (v, st)) in cols.iter().zip(values) {
        let (pos, align) = if c.right {
            (
                pos2(x + c.width - theme::S8, r.center().y),
                Align2::RIGHT_CENTER,
            )
        } else {
            (pos2(x, r.center().y), Align2::LEFT_CENTER)
        };
        text.paint(&p, pos, align, v, *st);
        x += c.width;
    }
}

fn opt_ms(v: Option<f64>) -> (String, Style) {
    match v {
        Some(v) if v.is_finite() => (format::ms(v), text::BODY),
        _ => ("—".into(), text::BODY_2),
    }
}

pub fn show(ui: &mut Ui, text: &Text, s: &Session, tab: &mut Tab) {
    let errors = s.errors.len();
    let labels = [
        format!("Requests {}", format::count(s.requests.len() as u64)),
        format!("Connections {}", format::count(s.conns.len() as u64)),
        format!("Errors {}", format::count(errors as u64)),
    ];
    let sel = match tab {
        Tab::Requests => 0,
        Tab::Connections => 1,
        Tab::Errors => 2,
    };
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    if let Some(i) = widgets::segmented(ui, text, &refs, sel, true) {
        *tab = [Tab::Requests, Tab::Connections, Tab::Errors][i];
    }
    ui.add_space(theme::S8);
    let wall = |t: f64| {
        s.wall_at(t)
            .map(|w| format::clock(w, true))
            .unwrap_or_default()
    };
    let row_h = theme::TABLE_ROW;
    match tab {
        Tab::Requests => {
            header(ui, text, &REQ_COLS);
            let n = s.requests.len();
            ScrollArea::vertical()
                .id_salt("req-log")
                .max_height(theme::LOG_HEIGHT)
                .auto_shrink([false, true])
                .show_rows(ui, row_h, n, |ui, range| {
                    for i in range {
                        let Some(r) = s.requests.get(n - 1 - i) else { continue };
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::SURFACE_RAISED);
                        }
                        let color = widgets::color_of_dir(r.dir);
                        let retrans = match r.dir {
                            Direction::Download => r.server_tcp.and_then(|t| t.retrans_packets),
                            Direction::Upload => r.client_tcp.and_then(|t| t.tx_retrans_packets),
                        };
                        let vals = [
                            (wall(r.t_start), text::BODY),
                            (r.dir.label().to_owned(), text::BODY.color(color)),
                            (size_label(r.size), text::BODY),
                            (format!("{}/{}", r.index, r.count), text::BODY),
                            (format!("#{}{}", r.conn, if r.new_conn { " new" } else { "" }), text::BODY),
                            opt_ms(Some(r.ttfb_ms)),
                            opt_ms(r.server_ms),
                            opt_ms(Some(r.transfer_ms)),
                            opt_ms(Some(r.duration_ms)),
                            match r.bps {
                                Some(b) => (format::mbps(b), text::BODY.color(color)),
                                None => ("—".into(), text::BODY_2),
                            },
                            match retrans {
                                Some(v) => (format::count(v), text::BODY),
                                None => ("—".into(), text::BODY_2),
                            },
                        ];
                        cells(ui, text, rect, &REQ_COLS, &vals);
                        if let Some(hp) = resp.hover_pos() {
                            let mut t = Tip::default().title(format!(
                                "{} {} · sample {}/{}",
                                size_label(r.size),
                                r.dir.lower(),
                                r.index,
                                r.count
                            ));
                            t.push_pair("Bytes", format::count(r.bytes));
                            if !r.complete {
                                t.push_pair("Complete", "No, time limit");
                            }
                            if let Some(st) = r.server_tcp {
                                if let Some(v) = st.rtt_us {
                                    t.push_pair("Edge TCP RTT", format!("{} ms", format::ms(v as f64 / 1000.0)));
                                }
                                if let Some(v) = st.cwnd {
                                    t.push_pair("Edge cwnd", format!("{} packets", format::count(v)));
                                }
                                if let Some(v) = st.sent_packets {
                                    t.push_pair("Edge packets sent", format::count(v));
                                }
                                if let Some(v) = st.lost_packets {
                                    t.push_pair("Edge packets lost", format::count(v));
                                }
                                if let Some(v) = st.retrans_packets {
                                    t.push_pair("Edge retransmits", format::count(v));
                                }
                            }
                            if let Some(ct) = r.client_tcp {
                                if let Some(v) = ct.srtt_ms {
                                    t.push_pair("Local TCP RTT", format!("{} ms", format::ms(v)));
                                }
                                if let Some(v) = ct.mss {
                                    t.push_pair("MSS", format!("{v} bytes"));
                                }
                                if let Some(v) = ct.tx_retrans_packets {
                                    t.push_pair("Local retransmits", format::count(v));
                                }
                                if let Some(v) = ct.rx_ooo_bytes {
                                    t.push_pair("Out-of-order bytes", format::count(v));
                                }
                            }
                            let t = t.note(
                                "Edge counters come from the response's cfL4 Server-Timing entry and are cumulative for the connection when the headers were sent. Local counters are this machine's kernel TCP statistics after the request.",
                            );
                            tooltip(ui, text, hp, &t);
                        }
                    }
                });
        }
        Tab::Connections => {
            header(ui, text, &CONN_COLS);
            let n = s.conns.len();
            ScrollArea::vertical()
                .id_salt("conn-log")
                .max_height(theme::LOG_HEIGHT)
                .auto_shrink([false, true])
                .show_rows(ui, row_h, n, |ui, range| {
                    for i in range {
                        let Some(c) = s.conns.get(n - 1 - i) else { continue };
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::SURFACE_RAISED);
                        }
                        let vals = [
                            (wall(c.t), text::BODY),
                            (c.purpose.label().to_owned(), text::BODY),
                            (c.remote.to_string(), text::BODY),
                            (c.local.to_string(), text::BODY),
                            opt_ms(Some(c.dns_ms)),
                            opt_ms(Some(c.connect_ms)),
                            opt_ms(Some(c.tls_ms)),
                            (c.tls_version.clone(), text::BODY),
                            (c.cipher.clone(), text::BODY),
                        ];
                        cells(ui, text, rect, &CONN_COLS, &vals);
                        if let Some(hp) = resp.hover_pos() {
                            let t = Tip::default()
                                .title(format!("Connection #{}", c.id))
                                .pair("ALPN", c.alpn.clone().unwrap_or_else(|| "—".into()))
                                .note("DNS: getaddrinfo. TCP: connect() to the first address of the chosen family. TLS: ClientHello sent to handshake complete (full handshake, no resumption).");
                            tooltip(ui, text, hp, &t);
                        }
                    }
                });
        }
        Tab::Errors => {
            let n = s.errors.len();
            if n == 0 {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                text.paint(
                    ui.painter(),
                    pos2(r.left(), r.center().y),
                    Align2::LEFT_CENTER,
                    "—",
                    text::BODY_2,
                );
                return;
            }
            ScrollArea::vertical()
                .id_salt("err-log")
                .max_height(theme::LOG_HEIGHT)
                .auto_shrink([false, true])
                .show_rows(ui, row_h, n, |ui, range| {
                    for i in range {
                        let Some((t, m)) = s.errors.get(n - 1 - i) else {
                            continue;
                        };
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                        let p = ui.painter_at(rect);
                        text.paint(
                            &p,
                            pos2(rect.left(), rect.center().y),
                            Align2::LEFT_CENTER,
                            &wall(*t),
                            text::BODY,
                        );
                        text.paint(
                            &p,
                            pos2(rect.left() + theme::COL_USE, rect.center().y),
                            Align2::LEFT_CENTER,
                            m,
                            text::BODY.color(theme::ERROR),
                        );
                    }
                });
        }
    }
}
