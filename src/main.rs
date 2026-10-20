//! Raw Speed Test: a fine-grained internet speed test against Cloudflare's
//! public speed test endpoints.

// No console window on Windows; other targets ignore this.
#![windows_subsystem = "windows"]

mod app;
mod engine;
mod model;
mod net;
mod platform;
mod shell;
mod stats;
mod ui;
// The native shells adopt the view layer one by one; until each does, the
// parts it would use count as unused.
#[allow(dead_code)]
mod view;

fn main() {
    platform::current().prepare_process();
    shell::run();
}
