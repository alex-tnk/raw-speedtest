// C ABI between the Rust core and the SwiftUI shell, implemented in
// src/shell/macos/ffi.rs (a test checks the two list the same functions).
// Every function runs on the main thread; only the callbacks are invoked
// from other threads. Returned buffers are borrowed until the next call of
// the same function (for scenes: of the same chart).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct RstCore RstCore;

/// Called from any thread when there is something new; pull a frame
/// within `delay_ms`. At most one wake is outstanding until the next pull.
typedef void (*RstWakeFn)(void *ctx, uint32_t delay_ms);
/// Called from the signal thread on SIGTERM/SIGINT/SIGHUP: quit normally.
typedef void (*RstQuitFn)(void *ctx);

typedef struct {
    const uint8_t *ptr;
    size_t len;
} RstBytes;

/// One drawing command of a chart scene.
///   kind 0 line:  points [start, end), width `a`, flags bit 0 = dashed
///   kind 1 area:  polyline [start, end) filled down to y = `a`
///   kind 2 rect:  x0, y0, x1, y1; flags bit 0 = filled, else 1 pt outline
///   kind 3 dots:  points [start, end), radius `a`
///   kind 4 text:  text bytes [start, end) at (x0, y0); flags: bits 0-1
///                 size (0 small, 1 body), bits 2-3 horizontal alignment
///                 (0 start, 1 center, 2 end), bits 4-5 vertical (0 top,
///                 1 center, 2 bottom)
/// `color` is a colour token (see rst_fixed); `alpha` multiplies it.
typedef struct {
    uint8_t kind;
    uint8_t color;
    uint8_t flags;
    uint8_t pad;
    float alpha;
    float a;
    uint32_t start;
    uint32_t end;
    float x0;
    float y0;
    float x1;
    float y1;
} RstCmd;

typedef struct {
    const RstCmd *cmds;
    uint32_t n_cmds;
    /// x, y pairs.
    const float *pts;
    uint32_t n_pts;
    /// UTF-8, not NUL-terminated.
    const uint8_t *text;
    uint32_t n_text;
    float plot_x0;
    float plot_y0;
    float plot_x1;
    float plot_y1;
    /// Changes whenever the scene was rebuilt.
    uint64_t generation;
} RstScene;

void rst_set_wake(RstCore *core, RstWakeFn wake, void *ctx);
void rst_set_quit(RstCore *core, RstQuitFn quit, void *ctx);
/// Clears whatever AppKit stored in the app's preferences (save panel
/// folder, menu bar item visibility). Call right after such a write.
void rst_forget_preferences(RstCore *core);

/// Data colours, log columns and chart constants, as JSON.
void rst_fixed(RstCore *core, RstBytes *out);
/// The frame as JSON (see swift/Model.swift). Clears the outstanding wake.
void rst_frame(RstCore *core, RstBytes *out);
/// Chart kinds: 0 throughput, 1 latency, 2 overview, 3 box plot `index`
/// (0 download, 1 upload, 2 latency, 3 gateway, 4 UDP), 4 RSSI.
bool rst_chart(RstCore *core, uint32_t kind, uint32_t index, float w, float h,
               float label_h, RstScene *out);
/// Hover at chart-local (x, y), as JSON.
bool rst_hover(RstCore *core, uint32_t kind, uint32_t index, float w, float h,
               float x, float y, RstBytes *out);
/// 0 start, 1 stop, 2 pause, 3 resume, 4 reset, 5 run once, 6 run
/// continuously, 7 show the whole run, 8 one-shot mode, 9 continuous mode.
void rst_command(RstCore *core, uint32_t cmd);
/// 0 zoom by `factor` at `x`, 1 pan by `x` points, 2 center the overview on
/// `x`; `w` is the chart's width.
void rst_view(RstCore *core, uint32_t op, double factor, float x, float w);
/// Choose option `index` of setting `setting`.
void rst_set(RstCore *core, uint32_t setting, uint32_t index);
/// Log rows [first, first + count) of tab 0 requests, 1 connections,
/// 2 errors, newest first, as JSON with the total row count.
void rst_log(RstCore *core, uint32_t tab, uint32_t first, uint32_t count, RstBytes *out);
/// Export every sample as JSON to a UTF-8 path; the result shows in the
/// status line.
void rst_export(RstCore *core, const uint8_t *path, size_t len);
