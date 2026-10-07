# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A tray-only desktop CCTV viewer for Dahua DVRs, built with Rust + Tauri 2. It streams RTSP channels through `ffmpeg` (transcoded to MJPEG) and renders them in a native WebView pointed at a local axum HTTP/WebSocket server. Chosen over Electron for the small binary and low RAM baseline — **RAM/CPU footprint is a primary design constraint, not an afterthought** (see "Resource discipline" below).

## Commands

```bash
cargo run -p ixdev-cctv              # dev run (debug, opens window + tray)
cargo build -p ixdev-cctv            # debug build
cargo build --release -p ixdev-cctv  # release build (LTO, stripped)
cargo tauri build                    # package .dmg/.app (needs: cargo install tauri-cli --version "^2")
```

Runtime config via env vars: `DVR_IP` (default `192.168.0.30`), `DVR_PORT` (`554`), `CAMERAS` (`8`), `FFMPEG_BIN` (override ffmpeg path).

Runtime log: `/tmp/ixdev-cctv.log` (auto-rotates at 5 MB). ffmpeg failures (incl. its stderr — auth/RTSP/decode errors) are logged there, not surfaced in the UI. A black grid with green status dots = WebSocket connected but ffmpeg produced no frames; check this log. A common cause is the Dahua DVR's concurrent-RTSP-connection limit (e.g. its web UI open at the same time, or leftover ffmpeg processes) → ffmpeg EOFs immediately.

There are no automated tests. Verify changes by running the app against a real or reachable DVR.

## Architecture

Three pieces, two processes-worth of state:

- **`src-tauri/src/main.rs`** — Tauri host: native window, system tray menu (quality presets, live CPU/RAM, refresh, show/quit), window-state persistence, and lifecycle. The X button **hides to tray** rather than quitting. On macOS the app runs as an `Accessory` (no Dock icon). Tray menu is fully rebuilt on every change (`rebuild_tray`) and stats refresh on a 10s background thread.
- **`src-tauri/src/server.rs`** — axum server bound to `127.0.0.1:0` (random port), running on its own Tokio runtime in a spawned thread. Serves the UI, auth/config/stats/quality endpoints, and `WS /ws/:ch` which spawns **one ffmpeg process per WebSocket connection**.
- **`frontend/index.html`** — single-file vanilla HTML/CSS/JS UI. 3×3 grid; click a cell for a fullscreen HD modal. Decodes MJPEG frames via `createImageBitmap` onto `<canvas>`.

Data flow: WebView → `http://127.0.0.1:PORT` → `ws_camera` → ffmpeg RTSP→MJPEG over stdout → JPEG framing → WebSocket binary → canvas.

State lives in `AppState` (in `server.rs`): `session` (DVR creds, set via `POST /login`, held in memory only), `quality`, and config. It is `Clone`d into both the Tauri setup closure and the axum router, sharing the same `Arc`s.

### Stream quality / sharpness model

**Default path is HEVC passthrough** (`?pass=1`): ffmpeg `-c:v copy` (DVR cameras are H.265) → Annex-B access units split on AUD NALs (`[key flag][AU]` per WS message) → WebCodecs `VideoDecoder` in the WebView (`playHevc` in `index.html`) → canvas. Native camera fps, ~0% ffmpeg CPU. On any failure (no VideoDecoder, decoder error, no frame in 4 s) the frontend sets `hevcBroken` and falls back to the MJPEG path described below. Grid fps is therefore the DVR sub-stream fps (set on the DVR), not `Quality::params`.


`Quality` (Low/Medium/High) × `hd` flag → `(fps, q:v)` via `Quality::params`:
- **Grid cells** request `hd=false` → RTSP `subtype=1` (the camera's **sub stream**, typically low-res CIF). Low fps; ffmpeg `-vf` is just `fps=N`.
- **Fullscreen modal** requests `?hd=1` → `subtype=0` (**main stream**, full res), higher fps.

The grid's sharpness ceiling is the camera's sub-stream resolution (a DVR-side setting), not the app. The grid `<canvas>` backing store is sized in **device pixels** (`devicePixelRatio`) to avoid a second upscale on HiDPI/Retina displays.

## Resource discipline (read before "optimizing")

Several non-obvious choices exist specifically to keep RAM/CPU down — especially the macOS WKWebView Networking process, which ballooned to 1+ GB before these were added. Don't undo them casually:

- WS frame channel has **capacity 1**; the ffmpeg reader **drops frames** rather than letting them queue (prevents WebView buffer pileup). Frontend mirrors this: at most 1 decode in-flight + 1 pending frame per camera; newest frame wins.
- Frontend uses `arraybuffer` (not Blob) WS binary type so memory releases from the Networking process immediately.
- ffmpeg runs with `-threads 1`, `nobuffer`/`low_delay`, small `probesize`.
- Grid fps is deliberately low; do **not** add upscaling to the grid ffmpeg pipeline (it re-bloats WS/WebView buffers).
- Frame buffer capped at `MAX_BUF` (512 KB); larger "frames" are treated as corrupt and dropped.

Deliberate simplifications are tagged with `ponytail:` comments — they mark intent, not bugs.

## Gotchas

- **ffmpeg must resolve at runtime.** `find_ffmpeg()` checks `FFMPEG_BIN`, then hardcoded Homebrew/system paths, then PATH. `main.rs` also prepends Homebrew paths to `PATH` because a Finder-launched `.app` gets a minimal PATH.
- **Vendored brotli patch.** `vendor/brotli/` is a patched copy applied via `[patch.crates-io]` in the root `Cargo.toml` to resolve an `alloc-no-stdlib` version conflict from `tauri-utils`. Remove only once upstream is fixed.
- Frontend is served via `include_str!` and the tray icon via `include_bytes!` — they're compiled in, so edits require a rebuild to take effect (no hot reload).
