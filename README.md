# ixdev-home-cctv (Rust / Tauri)

A lightweight desktop CCTV viewer for Dahua DVR systems — built with Rust + Tauri 2.  
Streams RTSP channels via ffmpeg, renders frames in a browser-based grid through a local WebSocket server.

> **Why Rust/Tauri over Electron?**  
> Electron bundles Chromium (~150 MB, ~250-400 MB RAM baseline).  
> Tauri uses the OS's native WebView — binary ~10 MB, RAM baseline ~80-150 MB.

---

## Features

- 3×3 camera grid (up to 8 cameras + 1 empty slot)
- Click any cell to open fullscreen HD stream
- System tray with live CPU / RAM stats, quality presets, refresh
- Hides to tray on window close (tray-only app, no Dock icon)
- Quality presets: Low / Medium / High (adjusts fps and JPEG quality)
- Pause streams automatically when window is hidden

---

## Architecture

```
┌─────────────────────────────────┐
│  Tauri (native window + tray)   │
│                                 │
│  ┌───────────────────────────┐  │
│  │  axum HTTP server         │  │
│  │  (runs on random port)    │  │
│  │                           │  │
│  │  GET  /              HTML │  │
│  │  GET  /api/config         │  │
│  │  POST /login              │  │
│  │  POST /logout             │  │
│  │  GET  /api/stats          │  │
│  │  GET  /api/quality        │  │
│  │  WS   /ws/:ch             │  │
│  │    └─ ffmpeg (per conn)   │  │
│  └───────────────────────────┘  │
│                                 │
│  WebView → http://127.0.0.1:PORT│
└─────────────────────────────────┘
```

---

## Prerequisites

### All platforms

| Tool | Version | Install |
|------|---------|---------|
| Rust | ≥ 1.77  | https://rustup.rs |
| ffmpeg | any modern | see below |

### macOS

```bash
# Homebrew
brew install ffmpeg

# Tauri system deps (WebKit is built-in on macOS — nothing extra needed)
```

### Linux (Ubuntu / Debian)

```bash
sudo apt update
sudo apt install -y \
    ffmpeg \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    patchelf
```

### Windows

1. Download ffmpeg from https://ffmpeg.org/download.html  
2. Add `ffmpeg.exe` to `PATH`  
3. Install [Microsoft Visual C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (required by Tauri)  
4. Install [WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (usually pre-installed on Windows 10/11)

---

## Development

```bash
# Clone
git clone https://github.com/<your-username>/ixdev-home-cctv-rust.git
cd ixdev-home-cctv-rust

# Run in dev mode (debug binary, no optimisations)
cargo run -p ixdev-cctv
```

The app starts, opens a window at a random local port, and creates a tray icon.  
Login with your DVR IP, username and password.

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DVR_IP` | `192.168.0.30` | Pre-fill DVR IP in login form |
| `DVR_PORT` | `554` | RTSP port |
| `CAMERAS` | `8` | Number of camera channels |

```bash
DVR_IP=192.168.1.100 CAMERAS=4 cargo run -p ixdev-cctv
```

---

## Build (release)

```bash
cargo build --release -p ixdev-cctv
```

Output binary: `target/release/ixdev-cctv` (macOS/Linux) or `target\release\ixdev-cctv.exe` (Windows)

### Package as installable app

Install Tauri CLI first:

```bash
cargo install tauri-cli --version "^2"
```

Then build the distributable:

```bash
# macOS → .dmg + .app
cargo tauri build

# Cross-compile targets (requires target toolchain installed)
cargo tauri build --target aarch64-apple-darwin   # Apple Silicon
cargo tauri build --target x86_64-apple-darwin    # Intel Mac
```

Packaged output is in `src-tauri/target/release/bundle/`.

---

## Project structure

```
ixdev-home-cctv-rust/
├── Cargo.toml                  # Workspace root (also holds release profile + brotli patch)
├── vendor/
│   └── brotli/                 # Patched brotli 8.0.3 (fixes alloc-no-stdlib conflict)
├── frontend/
│   └── index.html              # Single-file UI (vanilla HTML/CSS/JS)
└── src-tauri/
    ├── Cargo.toml              # tauri + axum + tokio + futures
    ├── build.rs                # Required by tauri-build
    ├── tauri.conf.json         # App metadata, bundle config
    ├── capabilities/
    │   └── default.json        # Tauri permission set
    ├── icons/
    │   ├── icon.icns           # macOS bundle icon
    │   ├── icon.ico            # Windows bundle icon
    │   ├── icon.png            # Linux bundle icon (RGBA 8-bit)
    │   └── tray-icon-44.png    # System tray icon (44 px, trimmed SVG export)
    └── src/
        ├── main.rs             # Tauri setup, tray, window, lifecycle
        └── server.rs           # axum HTTP + WebSocket + ffmpeg streaming
```

---

## Known issues / notes

### `brotli` dependency conflict

`brotli 8.0.3` (pulled in by `tauri-utils`) brings in two incompatible versions of  
`alloc-no-stdlib` (2.x direct + 3.x via `alloc-stdlib`).  
A patched copy of brotli with the dependency corrected to `alloc-no-stdlib = "3"` is  
vendored at `vendor/brotli/` and applied via `[patch.crates-io]` in `Cargo.toml`.  
Once the upstream crate is fixed this vendor directory can be removed.

### ffmpeg must be in PATH

The app calls `ffmpeg` by name. If it is not in `PATH`, camera streams will silently fail.  
You can override the search path with the `FFMPEG_BIN` env var (planned, not yet implemented).

---

## License

MIT
