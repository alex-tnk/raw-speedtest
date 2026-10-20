//! Raw Speed Test: a fine-grained internet speed test against Cloudflare's
//! public speed test endpoints.

mod app;
mod engine;
mod model;
mod net;
mod platform;
mod shell;
mod stats;
mod ui;

fn main() {
    platform::current().prepare_process();
    shell::run();
}
