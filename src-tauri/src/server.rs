use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::{io::AsyncReadExt, net::TcpListener, time};

// ── Constants ─────────────────────────────────────────────────────────────────

const SOI: [u8; 2]    = [0xff, 0xd8];
const EOI: [u8; 2]    = [0xff, 0xd9];
// ponytail: 512KB per stream — frames >512KB are corrupt anyway; was 2MB
const MAX_BUF: usize  = 512 * 1024;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub dvr_user: String,
    pub dvr_pass: String,
    pub dvr_ip:   String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low    => "Low",
            Self::Medium => "Medium",
            Self::High   => "High",
        }
    }

    // Returns (fps, q:v) for grid (hd=false) or fullscreen (hd=true)
    fn params(self, hd: bool) -> (u32, u32) {
        match (self, hd) {
            // ponytail: grid fps cut ~40% to reduce data flooding the macOS WKWebView Networking process
            (Self::Low,    false) => (3,  8),
            (Self::Low,    true)  => (12, 5),
            (Self::Medium, false) => (5,  6),
            (Self::Medium, true)  => (15, 3),
            (Self::High,   false) => (8,  3),
            (Self::High,   true)  => (20, 2),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub session:    Arc<RwLock<Option<Session>>>,
    pub quality:    Arc<RwLock<Quality>>,
    pub cameras:    usize,
    pub slots:      usize,
    pub dvr_port:   u16,
    pub ffmpeg_bin: Arc<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session:    Arc::new(RwLock::new(None)),
            quality:    Arc::new(RwLock::new(Quality::Medium)),
            cameras:    env_usize("CAMERAS", 8),
            slots:      9,
            dvr_port:   env_u16("DVR_PORT", 554),
            ffmpeg_bin: Arc::new(find_ffmpeg()),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn log(msg: &str) {
    use std::io::Write;
    eprintln!("{msg}");
    const LOG_PATH: &str = "/tmp/ixdev-cctv.log";
    const MAX_LOG:  u64  = 5 * 1024 * 1024; // 5 MB — rotate when exceeded
    if std::fs::metadata(LOG_PATH).map(|m| m.len()).unwrap_or(0) > MAX_LOG {
        let _ = std::fs::remove_file(LOG_PATH);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

fn find_ffmpeg() -> String {
    // Allow explicit override — useful when ffmpeg lives in a non-standard location.
    if let Ok(v) = std::env::var("FFMPEG_BIN") {
        if !v.trim().is_empty() {
            log(&format!("[ffmpeg] using FFMPEG_BIN override: {v}"));
            return v;
        }
    }

    // Explicit absolute paths come first so the bundled .app (which has a
    // minimal PATH with no Homebrew) can still find ffmpeg.
    let candidates: &[&str] = if cfg!(windows) {
        &[
            "C:\\ffmpeg\\bin\\ffmpeg.exe",
            "ffmpeg.exe",
        ]
    } else {
        &[
            "/opt/homebrew/bin/ffmpeg",   // macOS Apple Silicon — Homebrew
            "/usr/local/bin/ffmpeg",      // macOS Intel — Homebrew / manual
            "/opt/local/bin/ffmpeg",      // MacPorts
            "/usr/bin/ffmpeg",            // Linux system
            "ffmpeg",                     // PATH fallback (works in dev shell)
        ]
    };

    for &c in candidates {
        let ok = std::process::Command::new(c)
            .args(["-version"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if ok {
            log(&format!("[ffmpeg] found: {c}"));
            return c.to_string();
        }
    }

    log("[ffmpeg] WARNING: not found in any known location — streams will fail");
    "ffmpeg".to_string()
}

fn find_bytes(hay: &[u8], needle: &[u8; 2], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(2).position(|w| w == needle).map(|p| p + from)
}

// Returns (cpu%, ram_mb) for the current process
pub fn get_stats() -> (f64, u64) {
    let pid = std::process::id();

    #[cfg(target_os = "linux")]
    {
        // Read VmRSS from /proc/self/status (KB → MB)
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            let ram = s
                .lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                / 1024;
            return (0.0, ram);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Sum RSS of all ixdev-cctv-related processes (main, Networking, Graphics and Media, AutoFill)
        if let Ok(out) = std::process::Command::new("ps")
            .args(["-A", "-o", "rss=,comm="])
            .output()
        {
            let ram: u64 = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains("ixdev-cctv"))
                .filter_map(|l| l.split_whitespace().next()?.parse::<u64>().ok())
                .sum::<u64>()
                / 1024;
            return (0.0, ram);
        }
    }

    let _ = pid;
    (0.0, 0)
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

async fn index_html() -> Html<&'static str> {
    Html(include_str!("../../frontend/index.html"))
}

#[derive(Serialize)]
struct ConfigResponse {
    dvr_ip:    String,
    cameras:   usize,
    slots:     usize,
    logged_in: bool,
}

async fn api_config(State(s): State<AppState>) -> Json<ConfigResponse> {
    let sess = s.session.read().unwrap();
    let dvr_ip = sess
        .as_ref()
        .map(|x| x.dvr_ip.clone())
        .or_else(|| std::env::var("DVR_IP").ok())
        .unwrap_or_else(|| "192.168.0.30".into());
    Json(ConfigResponse {
        dvr_ip,
        cameras:   s.cameras,
        slots:     s.slots,
        logged_in: sess.is_some(),
    })
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    dvr_ip:   String,
}

#[derive(Serialize)]
struct ApiResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    ok:    Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn login(
    State(s): State<AppState>,
    Json(body): Json<LoginBody>,
) -> impl IntoResponse {
    if body.username.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResult { ok: None, error: Some("Please enter Username".into()) }),
        );
    }
    if body.dvr_ip.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResult { ok: None, error: Some("Please enter DVR IP Address".into()) }),
        );
    }
    *s.session.write().unwrap() = Some(Session {
        dvr_user: body.username.trim().into(),
        dvr_pass: body.password,
        dvr_ip:   body.dvr_ip.trim().into(),
    });
    (axum::http::StatusCode::OK, Json(ApiResult { ok: Some(true), error: None }))
}

async fn logout(State(s): State<AppState>) -> Json<ApiResult> {
    *s.session.write().unwrap() = None;
    Json(ApiResult { ok: Some(true), error: None })
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn api_stats() -> Json<serde_json::Value> {
    let (cpu, ram) = get_stats();
    Json(serde_json::json!({ "cpu": cpu, "ram": ram }))
}

async fn api_quality(State(s): State<AppState>) -> Json<serde_json::Value> {
    let q = s.quality.read().unwrap().label().to_lowercase();
    Json(serde_json::json!({ "quality": q }))
}

// ── WebSocket / ffmpeg ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WsQuery {
    hd: Option<String>,
}

async fn ws_camera(
    ws:              WebSocketUpgrade,
    Path(ch):        Path<u32>,
    Query(q):        Query<WsQuery>,
    State(state):    State<AppState>,
) -> impl IntoResponse {
    let hd = q.hd.as_deref() == Some("1");
    ws.on_upgrade(move |sock| camera_stream(sock, ch, hd, state))
}

async fn camera_stream(socket: WebSocket, ch: u32, hd: bool, state: AppState) {
    let session = match state.session.read().unwrap().clone() {
        Some(s) => s,
        None    => return,
    };
    if ch < 1 || ch > state.cameras as u32 {
        return;
    }

    let (fps, qv) = state.quality.read().unwrap().params(hd);
    let subtype   = if hd { 0u8 } else { 1u8 };
    let url = format!(
        "rtsp://{}:{}@{}:{}/cam/realmonitor?channel={}&subtype={}",
        session.dvr_user, session.dvr_pass, session.dvr_ip, state.dvr_port, ch, subtype,
    );
    let vf = format!("fps={fps}");

    let mut child = match tokio::process::Command::new(state.ffmpeg_bin.as_str())
        .args([
            "-loglevel",        "quiet",
            "-fflags",          "nobuffer",
            "-flags",           "low_delay",
            "-probesize",       "1024",
            "-analyzeduration", "1000000",
            "-rtsp_transport",  "tcp",
            "-i",               &url,
            "-f",               "mjpeg",
            "-vf",              &vf,
            "-q:v",             &qv.to_string(),
            "-threads",         "1",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(c)  => c,
        Err(e) => {
            log(&format!("[ffmpeg] spawn error ch={ch} bin={}: {e}", state.ffmpeg_bin));
            return;
        }
    };

    let mut stdout = child.stdout.take().unwrap();
    let (mut tx, mut rx) = socket.split();

    // ponytail: capacity=1 — reader drops frames rather than letting them pile up in
    // the macOS WKWebView Networking process buffers (which caused the 1+ GB usage).
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);

    // WS sender task: pulls one frame at a time, 5 s timeout guards dead connections
    let sender = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            match time::timeout(Duration::from_secs(5), tx.send(Message::Binary(frame))).await {
                Ok(Ok(_)) => {}
                _         => break,
            }
        }
    });

    // Detect WebSocket close from the client side
    let (close_tx, mut close_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = rx.next().await {
            if matches!(msg, Message::Close(_)) { break; }
        }
        let _ = close_tx.send(());
    });

    // Pre-allocated rolling buffer — no Buffer.concat churn
    let mut buf = Vec::<u8>::with_capacity(MAX_BUF);
    let mut tmp = vec![0u8; 65536];

    'outer: loop {
        tokio::select! {
            biased;
            _ = &mut close_rx => break,
            result = stdout.read(&mut tmp) => {
                let n = match result {
                    Ok(0) => { log(&format!("[ffmpeg] stdout EOF ch={ch}")); break; }
                    Err(e) => { log(&format!("[ffmpeg] stdout read error ch={ch}: {e}")); break; }
                    Ok(n)  => n,
                };

                if buf.len() + n > MAX_BUF { buf.clear(); }
                buf.extend_from_slice(&tmp[..n]);

                // Parse all complete JPEG frames from buf
                loop {
                    let Some(s) = find_bytes(&buf, &SOI, 0) else { buf.clear(); break; };
                    let Some(e) = find_bytes(&buf, &EOI, s + 2) else {
                        if s > 0 { buf.drain(..s); }
                        break;
                    };
                    let frame = buf[s..e + 2].to_vec();
                    buf.drain(..e + 2);
                    match frame_tx.try_send(frame) {
                        Ok(_)                                                      => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_))      => {} // sender busy — drop frame
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_))    => break 'outer, // WS dead
                    }
                }
            }
        }
    }

    drop(frame_tx);
    sender.abort();
    let _ = child.kill().await;
}

// ── Server bootstrap ──────────────────────────────────────────────────────────

/// Starts the axum server on a background thread with its own Tokio runtime.
/// Returns the bound port so the caller can point a WebView at it.
pub fn start(state: AppState) -> u16 {
    let (port_tx, port_rx) = std::sync::mpsc::sync_channel::<u16>(1);

    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let router = Router::new()
                    .route("/",            get(index_html))
                    .route("/api/config",  get(api_config))
                    .route("/login",       post(login))
                    .route("/logout",      post(logout))
                    .route("/health",      get(health))
                    .route("/api/stats",   get(api_stats))
                    .route("/api/quality", get(api_quality))
                    .route("/ws/:ch",      get(ws_camera))
                    .with_state(state);

                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port     = listener.local_addr().unwrap().port();
                port_tx.send(port).unwrap();

                axum::serve(listener, router).await.unwrap();
            });
    });

    port_rx.recv().unwrap()
}
