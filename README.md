# Raw Speed Test

A fine-grained internet speed test for the desktop. It measures against
Cloudflare's public speed test endpoints and shows every sample it takes:
throughput in 50 ms bins, every latency probe, every UDP probe, per-request
DNS/TCP/TLS/TTFB timing, TCP retransmissions, gateway latency and Wi-Fi
signal. It runs once, or continuously for days with bounded memory.

The interface is native on each platform: SwiftUI with Liquid Glass on
macOS, Win32 with Mica on Windows, GTK 4 with libadwaita on Linux. It
follows the system's light or dark appearance.

On macOS it can also live in the menu bar (Settings → Run in menu bar), and
on Windows in the notification area: closing the window keeps the test
running, with live latency and a menu of the current values and run
controls.

The app writes nothing to disk. Data leaves it only through Export, to a
file you choose.

## Requirements

- **macOS 26** or later. Building needs the Xcode Command Line Tools
  (`xcode-select --install`); Xcode itself is not needed.
- **Windows 10 or 11**; Mica behind the title bar needs Windows 11 22H2.
- **Linux** with GTK 4.12 and libadwaita 1.5 or newer (Ubuntu 24.04,
  Fedora 40 or later); building needs their development packages
  (`libgtk-4-dev libadwaita-1-dev` on Debian/Ubuntu).
- A Rust toolchain, 1.95 or newer.

## Build

```sh
cargo run                               # development build, native UI
packaging/macos/build-app.sh            # target/release/Raw Speed Test.app
packaging/macos/build-app.sh universal  # arm64 + x86_64 .app
cargo build --release                   # Windows / Linux: one executable
```

macOS is the primary platform. On Linux, Wi-Fi details are reported as
unavailable. On Windows the measurements run, but TCP counters, interface
details, Wi-Fi and gateway latency are not implemented yet and show as
unavailable. The Windows and Linux interfaces have not been run on real
machines yet; `RST_EGUI=1` starts the previous cross-platform interface.

## What it measures

- **Latency:** time to first byte of `GET /__down?bytes=0` minus the server
  time the edge reports, unloaded and while downloading or uploading.
- **Jitter:** mean absolute difference between consecutive latency probes.
- **Download / upload:** sustained transfers: several connections (4 by
  default) kept busy with back-to-back requests for a fixed time, 10 s per
  direction in a one-shot test. Throughput is the bytes moved on all
  connections after the first 2 s (TCP ramp-up) divided by that time, and
  is also plotted in 50 ms bins.
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
```
