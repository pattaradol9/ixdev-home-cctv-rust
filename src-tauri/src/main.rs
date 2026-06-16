// Suppress the extra console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;

use server::{get_stats, AppState, Quality};
use dpi::{PhysicalPosition, PhysicalSize};
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};

// ── Window state persistence ───────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct WindowState {
    x:      i32,
    y:      i32,
    width:  u32,
    height: u32,
}

fn window_state_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_local_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("window-state.json"))
}

fn load_window_state(app: &tauri::AppHandle) -> Option<WindowState> {
    let data = std::fs::read_to_string(window_state_path(app)?).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_window_state(window: &tauri::WebviewWindow) {
    let Ok(pos)  = window.outer_position() else { return };
    let Ok(size) = window.inner_size()      else { return };
    let state    = WindowState { x: pos.x, y: pos.y, width: size.width, height: size.height };
    let Some(path) = window_state_path(window.app_handle()) else { return };
    if let Ok(json) = serde_json::to_string(&state) { let _ = std::fs::write(path, json); }
}

// ── Tray menu ─────────────────────────────────────────────────────────────────

fn build_menu(app: &tauri::AppHandle, state: &AppState) -> tauri::Result<Menu<tauri::Wry>> {
    let quality = *state.quality.read().unwrap();
    let session = state.session.read().unwrap().clone();

    let mut top: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = Vec::new();

    if let Some(ref sess) = session {
        let (cpu, ram) = get_stats();
        top.push(Box::new(MenuItem::with_id(app, "info-dvr", format!("DVR:  {}", sess.dvr_ip),  false, None::<&str>)?));
        top.push(Box::new(MenuItem::with_id(app, "info-cpu", format!("CPU:  {:.1}%", cpu),       false, None::<&str>)?));
        top.push(Box::new(MenuItem::with_id(app, "info-ram", format!("RAM:  {} MB", ram),         false, None::<&str>)?));
        top.push(Box::new(PredefinedMenuItem::separator(app)?));
    }

    top.push(Box::new(MenuItem::with_id(app, "show",    "Show Window",         true, None::<&str>)?));
    top.push(Box::new(PredefinedMenuItem::separator(app)?));

    let q_sub = Submenu::with_items(app, "Video Quality", true, &[
        &CheckMenuItem::with_id(app, "q-low",    "Low",    true, quality == Quality::Low,    None::<&str>)?,
        &CheckMenuItem::with_id(app, "q-medium", "Medium", true, quality == Quality::Medium, None::<&str>)?,
        &CheckMenuItem::with_id(app, "q-high",   "High",   true, quality == Quality::High,   None::<&str>)?,
    ])?;

    top.push(Box::new(q_sub));
    top.push(Box::new(MenuItem::with_id(app, "refresh", "Refresh All Cameras", true, None::<&str>)?));
    top.push(Box::new(PredefinedMenuItem::separator(app)?));
    top.push(Box::new(MenuItem::with_id(app, "quit",    "Quit",                true, None::<&str>)?));

    let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = top.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

fn rebuild_tray(app: &tauri::AppHandle, state: &AppState) {
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_menu(app, state) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn refresh_cameras(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval("window.refreshAllCameras && window.refreshAllCameras()");
    }
}

fn set_quality(app: &tauri::AppHandle, state: &AppState, q: Quality) {
    *state.quality.write().unwrap() = q;
    rebuild_tray(app, state);
    refresh_cameras(app);
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    // When launched as a .app bundle from Finder/Dock, macOS sets PATH to
    // /usr/bin:/bin:/usr/sbin:/sbin — Homebrew paths are missing.
    // Prepend them so that ffmpeg (and any child process) can be resolved.
    #[cfg(not(windows))]
    {
        let cur = std::env::var("PATH").unwrap_or_default();
        if !cur.contains("/opt/homebrew/bin") {
            std::env::set_var(
                "PATH",
                format!("/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:{cur}"),
            );
        }
    }

    let state = AppState::new();

    tauri::Builder::default()
        .setup({
            let state = state.clone();
            move |app| {
                // ── Hide from macOS Dock (tray-only app) ──────────────────
                #[cfg(target_os = "macos")]
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);

                // ── HTTP server ───────────────────────────────────────────
                let port = server::start(state.clone());

                // ── Main window ───────────────────────────────────────────
                let url = tauri::WebviewUrl::External(
                    url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
                );
                let window = tauri::WebviewWindowBuilder::new(app, "main", url)
                    .title("ixdev-cctv")
                    .inner_size(1280.0, 800.0)
                    .min_inner_size(320.0, 240.0)
                    .build()?;

                // Restore saved position/size using physical coordinates to avoid
                // any DPI/scale-factor conversion ambiguity in the builder.
                if let Some(ws) = load_window_state(app.handle()) {
                    let _ = window.set_position(PhysicalPosition::new(ws.x, ws.y));
                    let _ = window.set_size(PhysicalSize::new(ws.width, ws.height));
                }

                // ── Tray icon (44 px for correct Retina @2x menu-bar size) ──
                let icon_bytes = include_bytes!("../icons/tray-icon-44.png");
                let icon       = Image::from_bytes(icon_bytes)?;
                let menu       = build_menu(app.handle(), &state)?;
                let state_tray = state.clone();

                TrayIconBuilder::with_id("main")
                    .icon(icon)
                    .tooltip("ixdev-cctv")
                    .menu(&menu)
                    .on_menu_event(move |app, event| {
                        match event.id().as_ref() {
                            "show" => {
                                if let Some(w) = app.get_webview_window("main") {
                                    let _ = w.show();
                                    let _ = w.set_focus();
                                }
                            }
                            "q-low"    => set_quality(app, &state_tray, Quality::Low),
                            "q-medium" => set_quality(app, &state_tray, Quality::Medium),
                            "q-high"   => set_quality(app, &state_tray, Quality::High),
                            "refresh"  => refresh_cameras(app),
                            "quit"     => {
                                if let Some(w) = app.get_webview_window("main") {
                                    save_window_state(&w);
                                }
                                app.exit(0);
                            }
                            _          => {}
                        }
                    })
                    .on_tray_icon_event(|tray, event| {
                        if matches!(
                            event,
                            tauri::tray::TrayIconEvent::Click {
                                button: tauri::tray::MouseButton::Left,
                                button_state: tauri::tray::MouseButtonState::Up,
                                ..
                            }
                        ) {
                            let app = tray.app_handle();
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    })
                    .build(app)?;

                // ── Periodic stats refresh every 10 s (only when logged in) ──
                let app_handle  = app.handle().clone();
                let state_stats = state.clone();
                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        if state_stats.session.read().unwrap().is_some() {
                            rebuild_tray(&app_handle, &state_stats);
                        }
                    }
                });

                Ok(())
            }
        })
        // X button → save state, hide to tray, don't quit
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    if let Some(w) = window.app_handle().get_webview_window("main") {
                        save_window_state(&w);
                    }
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error running ixdev-cctv");
}
