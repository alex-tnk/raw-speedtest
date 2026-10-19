# Raw Speed Test

A fine-grained internet speed test for the desktop. It measures against
Cloudflare's public speed test endpoints and shows every sample it takes:
throughput in 50 ms bins, every latency probe, every UDP probe, per-request
DNS/TCP/TLS/TTFB timing, TCP retransmissions, gateway latency and Wi-Fi
signal. It runs once, or continuously for days with bounded memory.

The app writes nothing to disk. Data leaves it only through Export, to a
file you choose.

## Build

Requires a Rust toolchain (1.95 or newer).

```sh
cargo run                               # development build
packaging/macos/build-app.sh            # target/release/Raw Speed Test.app
packaging/macos/build-app.sh universal  # arm64 + x86_64 .app
```

Windows and Linux builds produce a single executable (`cargo build
--release`). macOS is the primary platform. On Linux, Wi-Fi details are
reported as unavailable and Export uses `zenity` or `kdialog`. On Windows
the measurements run, but TCP counters, interface details, Wi-Fi, gateway
latency and Export are not implemented yet and show as unavailable.

## What it measures

- **Latency:** time to first byte of `GET /__down?bytes=0` minus the server
  time the edge reports, unloaded and while downloading or uploading.
- **Jitter:** mean absolute difference between consecutive latency probes.
- **Download / upload:** per request, as Cloudflare's speed test computes it;
  the headline is the 90th percentile of requests lasting at least 10 ms.
- **Packet loss:** UDP STUN Binding requests to `stun.cloudflare.com:3478`
  without an answer within 3 s, unloaded and under load.
- **Connection:** DNS, TCP connect and TLS handshake per connection; public
  IP, ASN and data center from Cloudflare's response headers; interface,
  MTU, gateway and its ICMP round trip; Wi-Fi RSSI, noise, channel and rate.

On macOS the gateway ping needs Local Network access (System Settings >
Privacy & Security > Local Network); without it the value shows as
unavailable.

## Tests

```sh
cargo test
cargo test live_one_shot -- --ignored --nocapture    # against the live endpoints
cargo test --release soak -- --ignored --nocapture   # 4 simulated days
cargo test --release every_plot_has_hover -- --ignored
```
