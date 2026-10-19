//! Raw Speed Test: a fine-grained internet speed test against Cloudflare's
//! public speed test endpoints.

// The layers are added bottom-up and nothing reaches them until the UI
// does, so until then their items and re-exports count as unused.
#![allow(dead_code, unused_imports)]

mod engine;
mod model;
mod net;
mod platform;
mod stats;

fn main() {}
