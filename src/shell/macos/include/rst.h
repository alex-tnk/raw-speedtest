// C ABI between the Rust core and the SwiftUI shell. Implemented in
// src/shell/macos/ffi.rs; keep the two in sync.
#pragma once
#include <stdint.h>

typedef struct RstCore RstCore;

/// Called from any thread when new data is available; the shell should pull
/// within `delay_ms`. At most one wake is outstanding until the next pull.
typedef void (*RstWakeFn)(void *ctx, uint32_t delay_ms);
/// Called from the signal thread on SIGTERM/SIGINT/SIGHUP: quit normally.
typedef void (*RstQuitFn)(void *ctx);

void rst_set_wake(RstCore *core, RstWakeFn wake, void *ctx);
void rst_set_quit(RstCore *core, RstQuitFn quit, void *ctx);

/// Clears whatever AppKit stored in the app's preferences (save panel
/// folder, menu bar item visibility). Call right after such a write.
void rst_forget_preferences(RstCore *core);

typedef struct {
    const float *xy;
    uint32_t n;
    uint64_t version;
} RstPoints;

void rst_spike_points(RstCore *core, RstPoints *out);
