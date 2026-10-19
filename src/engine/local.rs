//! The local side of the path: interface, default gateway latency, Wi-Fi.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::RunCtx;
use super::latency::LOADED_INTERVAL;
use crate::model::{Event, GatewaySample, InterfaceInfo, Load, RunMode, WifiInfo, WifiSample};
use crate::platform::{self, PingError};

const PING_TIMEOUT: Duration = Duration::from_secs(1);
/// Gateway ping spacing on an idle link in one-shot mode.
const ONE_SHOT_IDLE: Duration = Duration::from_millis(500);
const WIFI_EVERY: Duration = Duration::from_secs(2);
const IFACE_EVERY: Duration = Duration::from_secs(10);

pub fn run(ctx: Arc<RunCtx>) {
    let p = platform::current();
    let mut iface: Option<InterfaceInfo> = None;
    let mut wifi: Option<Option<WifiInfo>> = None;
    let mut next_iface = Instant::now();
    let mut next_wifi = Instant::now();
    let mut next_ping = Instant::now();
    let mut seq: u16 = 0;
    let mut ping_supported = true;
    let mut ping_error_shown = false;
    loop {
        if !ctx.wait_running() || ctx.plan_done() {
            return;
        }
        let now = Instant::now();
        if now >= next_iface {
            match ctx.local_ip() {
                Some(ip) => {
                    let info = p.interface_for(ip);
                    if iface.as_ref() != Some(&info) {
                        ctx.emit(Event::Interface(info.clone()));
                        iface = Some(info);
                    }
                    next_iface = now + IFACE_EVERY;
                }
                None => next_iface = now + Duration::from_millis(200),
            }
        }
        if now >= next_wifi && iface.is_some() {
            let name = iface.as_ref().and_then(|i| i.name.clone());
            let w = p.wifi(name.as_deref());
            if let Some(w) = &w {
                ctx.emit(Event::WifiSample(WifiSample {
                    t: ctx.now(),
                    rssi_dbm: w.rssi_dbm,
                    noise_dbm: w.noise_dbm,
                    tx_rate_mbps: w.tx_rate_mbps,
                }));
            }
            if wifi.as_ref() != Some(&w) {
                ctx.emit(Event::Wifi(w.clone()));
                wifi = Some(w);
            }
            next_wifi = now + WIFI_EVERY;
        }
        let gateway = iface.as_ref().and_then(|i| i.gateway);
        if now >= next_ping
            && ping_supported
            && let Some(gw) = gateway
        {
            {
                let load = ctx.load();
                seq = seq.wrapping_add(1);
                let t = ctx.now();
                let r = p.ping(gw, seq, PING_TIMEOUT);
                let ms = match r {
                    Ok(d) => Some(d.as_secs_f64() * 1000.0),
                    Err(PingError::Timeout) => None,
                    Err(PingError::Unsupported) => {
                        ping_supported = false;
                        continue;
                    }
                    Err(PingError::Failed(e)) => {
                        if !ping_error_shown {
                            ctx.error(format!("Gateway ping failed: {e}"));
                            ping_error_shown = true;
                        }
                        next_ping = Instant::now() + Duration::from_secs(30);
                        continue;
                    }
                };
                ctx.emit(Event::Gateway(GatewaySample { t, ms, load }));
                let spacing = match (load, ctx.mode) {
                    (Load::Idle, RunMode::OneShot) => ONE_SHOT_IDLE,
                    (Load::Idle, RunMode::Continuous) => ctx.config().continuous.latency_interval,
                    _ => LOADED_INTERVAL,
                };
                next_ping = now + spacing;
            }
        }
        let next = next_iface.min(next_wifi).min(next_ping);
        let wait = next
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(10), Duration::from_millis(500));
        ctx.sleep_or_load_change(wait);
    }
}
