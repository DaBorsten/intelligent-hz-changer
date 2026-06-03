mod display;
mod identify;
mod process_icon;
mod process_watcher;
mod settings;
mod watcher;

use std::collections::HashMap;
use std::sync::Arc;
use tauri::tray::TrayIconId;
use tauri::{Emitter, Manager, Theme, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use display::MonitorInfoExtended;
use process_watcher::{WatchConfig, WatchState};

struct AppState {
    watch_state: Arc<WatchState>,
    tray_id: std::sync::OnceLock<TrayIconId>,
    close_to_tray: std::sync::atomic::AtomicBool,
}

#[tauri::command]
fn set_window_theme(app: tauri::AppHandle, theme: String) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let t = match theme.as_str() {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    };
    window.set_theme(t).map_err(|e| e.to_string())
}

// ── Tauri Commands ────────────────────────────────────────────────────────────

#[tauri::command]
fn get_monitors() -> Vec<display::MonitorInfo> {
    display::enumerate_monitors()
}

#[tauri::command]
fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
    display::get_monitors_extended()
}

#[tauri::command]
fn get_supported_hz(monitor_name: String) -> Vec<u32> {
    display::get_supported_refresh_rates(&monitor_name)
}

#[tauri::command]
fn get_current_hz(monitor_name: String) -> u32 {
    display::get_current_refresh_rate(&monitor_name)
}

/// Monotonic token so only the most recent `test_hz` call reverts the rate.
/// Without it, overlapping tests would each restore their own stale "current"
/// value after 5 s, clobbering one another.
static TEST_HZ_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[tauri::command]
fn test_hz(
    monitor_name: String,
    hz: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let ws = Arc::clone(&state.watch_state);
    let current = {
        let _guard = ws.hz_lock.lock().unwrap_or_else(|e| e.into_inner());
        let current = display::get_current_refresh_rate(&monitor_name);
        display::set_refresh_rate(&monitor_name, hz)?;
        current
    };
    let token = TEST_HZ_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let mn = monitor_name.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if TEST_HZ_TOKEN.load(std::sync::atomic::Ordering::Relaxed) != token {
            return; // a newer test superseded this one
        }
        // Don't revert if the watcher now owns the rate (a watched process is
        // running) — that value must win over our stale pre-test snapshot.
        if ws.is_any_running() {
            return;
        }
        let _guard = ws.hz_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = display::set_refresh_rate(&mn, current);
    });
    Ok(())
}

#[tauri::command]
fn identify_monitors(theme: Option<String>) {
    let monitors = display::get_monitors_extended();
    let is_light = theme.as_deref() == Some("light");
    identify::show_overlays(monitors, is_light);
}

#[tauri::command]
fn get_process_counts(state: tauri::State<'_, AppState>) -> HashMap<String, u32> {
    state.watch_state.get_process_counts()
}

#[tauri::command]
fn load_config(app: tauri::AppHandle) -> Result<WatchConfig, String> {
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("config.json");

    if path.exists() {
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    } else {
        Ok(WatchConfig::default())
    }
}

#[tauri::command]
fn save_config(
    watched_processes: Vec<String>,
    monitor_name: String,
    game_hz: u32,
    default_hz: u32,
    monitor_settings: Option<HashMap<String, process_watcher::MonitorHz>>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let config = WatchConfig {
        watched_processes,
        monitor_name,
        game_hz,
        default_hz,
        monitor_settings: monitor_settings.unwrap_or_default(),
    };

    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("config.json"), json).map_err(|e| e.to_string())?;

    let should_reset = state.watch_state.update_config(config);
    if should_reset && state.watch_state.is_enabled() {
        watcher::sync_hz(
            &state.watch_state,
            &app,
            "Prozess aus Liste entfernt (kein aktiver Prozess mehr)".into(),
            None,
            "process_stop",
        );
    }
    // A process just added to the list may already be running — no WMI creation
    // event will ever fire for it, so reconcile the running set once now.
    #[cfg(windows)]
    watcher::reconcile(&state.watch_state, &app);
    Ok(())
}

#[tauri::command]
fn set_enabled(
    value: bool,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    state.watch_state.set_enabled(value);
    persist_enabled(&app, value);

    // Reflect pause/resume on the hardware immediately: resuming re-applies the
    // correct rate for the current running set; pausing restores the default so
    // the user isn't left stuck at game Hz while the watcher is inactive.
    if value {
        watcher::sync_hz(&state.watch_state, &app, "Aktiviert".into(), None, "system");
    } else {
        let (monitor, def) = {
            let cfg = state.watch_state.config.lock().unwrap_or_else(|e| e.into_inner());
            let m = cfg.monitor_name.clone();
            let d = cfg.default_hz_for(&m);
            (m, d)
        };
        watcher::set_monitor_hz(
            &state.watch_state,
            &app,
            &monitor,
            def,
            "Pausiert".into(),
            None,
            "system",
        );
    }

    // Update tray tooltip + menu item label
    if let Some(tray_id) = state.tray_id.get() {
        if let Some(tray) = app.tray_by_id(tray_id) {
            let _ = tray.set_tooltip(Some(if value {
                "Intelligent Hz Changer – aktiv"
            } else {
                "Intelligent Hz Changer – pausiert"
            }));
            // Rebuild menu with updated toggle label
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            let toggle_label = if value { "Deaktivieren" } else { "Aktivieren" };
            if let (Ok(toggle), Ok(sep), Ok(open), Ok(quit)) = (
                MenuItem::with_id(&app, "toggle", toggle_label, true, None::<&str>),
                PredefinedMenuItem::separator(&app),
                MenuItem::with_id(&app, "open", "App öffnen", true, None::<&str>),
                MenuItem::with_id(&app, "quit", "Beenden", true, None::<&str>),
            ) {
                if let Ok(menu) = Menu::with_items(&app, &[&toggle, &sep, &open, &quit]) {
                    let _ = tray.set_menu(Some(menu));
                }
            }
        }
    }

    app.emit("enabled-changed", value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_enabled(state: tauri::State<'_, AppState>) -> bool {
    state.watch_state.is_enabled()
}

#[tauri::command]
fn get_running_watched(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.watch_state.get_running()
}

fn settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .map_err(|e| e.to_string())
}

fn read_settings(app: &tauri::AppHandle) -> settings::AppSettings {
    settings_path(app)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Persists only the `enabled` flag without disturbing other settings.
fn persist_enabled(app: &tauri::AppHandle, value: bool) {
    let mut s = read_settings(app);
    s.enabled = value;
    if let Ok(dir) = app.path().app_config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(&s) {
            let _ = std::fs::write(dir.join("settings.json"), json);
        }
    }
}

#[tauri::command]
fn load_settings(app: tauri::AppHandle) -> settings::AppSettings {
    let mut s = if let Ok(path) = settings_path(&app) {
        if let Ok(json) = std::fs::read_to_string(&path) {
            serde_json::from_str(&json).unwrap_or_default()
        } else {
            settings::AppSettings::default()
        }
    } else {
        settings::AppSettings::default()
    };

    // Always reflect the real registry state, not the stored value.
    // This way external changes (e.g. Task Manager autostart toggle) are shown correctly.
    #[cfg(windows)]
    {
        s.autostart = settings::get_autostart("IntelligentHzChanger");
    }

    s
}

#[tauri::command]
fn save_settings(
    mut s: settings::AppSettings,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // `enabled` is owned by set_enabled/the tray, not the settings UI — keep the
    // live runtime value so a settings save can never silently re-enable the watcher.
    s.enabled = state.watch_state.is_enabled();

    // Persist close_to_tray in runtime state
    state.close_to_tray.store(
        s.close_to_tray,
        std::sync::atomic::Ordering::Relaxed,
    );

    // Autostart via Windows registry
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        settings::set_autostart("IntelligentHzChanger", &exe, s.autostart)?;
    }

    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("settings.json"), json).map_err(|e| e.to_string())?;
    Ok(())
}


#[tauri::command]
async fn get_process_icon(process_name: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let exe_path = find_exe_path(&process_name)?;
            process_icon::extract_icon_base64(&exe_path)
        }
        #[cfg(not(windows))]
        {
            let _ = process_name;
            None
        }
    })
    .await
    .unwrap_or(None)
}

/// Snapshot of running processes via WMI: (Name, ExecutablePath).
/// Replaces spawning `powershell.exe` — no process startup cost and no string
/// interpolation, so a process name can never be used for command injection.
#[cfg(windows)]
fn query_processes() -> Vec<(String, Option<String>)> {
    use serde::Deserialize;
    use wmi::{COMLibrary, WMIConnection};

    #[derive(Deserialize)]
    #[serde(rename = "Win32_Process")]
    #[serde(rename_all = "PascalCase")]
    struct Proc {
        name: String,
        executable_path: Option<String>,
    }

    // spawn_blocking threads may be reused and already COM-initialized.
    let com = COMLibrary::new().unwrap_or_else(|_| unsafe { COMLibrary::assume_initialized() });
    let Ok(con) = WMIConnection::new(com) else {
        return vec![];
    };
    let results: Result<Vec<Proc>, _> = con.query();
    results
        .map(|v| v.into_iter().map(|p| (p.name, p.executable_path)).collect())
        .unwrap_or_default()
}

#[tauri::command]
async fn get_all_running_processes() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            let mut names: Vec<String> = query_processes()
                .into_iter()
                .map(|(n, _)| n)
                .filter(|n| !n.is_empty())
                .collect();
            names.sort_unstable_by_key(|n| n.to_lowercase());
            names.dedup_by_key(|n| n.to_lowercase());
            names
        }
        #[cfg(not(windows))]
        Vec::<String>::new()
    })
    .await
    .unwrap_or_default()
}

/// Resolves a process name to its executable path via WMI.
/// `ExecutablePath` is populated regardless of privilege level (unlike
/// `Get-Process .Path`, which is null for elevated processes like Vanguard).
#[cfg(windows)]
fn find_exe_path(process_name: &str) -> Option<String> {
    query_processes()
        .into_iter()
        .find(|(name, path)| {
            name.eq_ignore_ascii_case(process_name)
                && path.as_deref().is_some_and(|p| !p.is_empty())
        })
        .and_then(|(_, path)| path)
}

#[tauri::command]
async fn show_update_notification(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    app.notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── App setup ─────────────────────────────────────────────────────────────────

fn load_config_from_disk(app: &tauri::AppHandle) -> WatchConfig {
    let Ok(dir) = app.path().app_config_dir() else {
        return WatchConfig::default();
    };
    let path = dir.join("config.json");
    if let Ok(json) = std::fs::read_to_string(path) {
        serde_json::from_str(&json).unwrap_or_default()
    } else {
        WatchConfig::default()
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

            // Load persisted settings up front so the tray reflects the restored state.
            let app_settings = read_settings(app.handle());

            // System tray — label shows the action, so it is inverted vs. enabled state.
            let toggle_label = if app_settings.enabled { "Deaktivieren" } else { "Aktivieren" };
            let toggle_item = MenuItem::with_id(app, "toggle", toggle_label, true, None::<&str>)?;
            let sep_item = PredefinedMenuItem::separator(app)?;
            let open_item =
                MenuItem::with_id(app, "open", "App öffnen", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle_item, &sep_item, &open_item, &quit_item])?;

            let mut tray_builder = TrayIconBuilder::new()
                .tooltip(if app_settings.enabled {
                    "Intelligent Hz Changer – aktiv"
                } else {
                    "Intelligent Hz Changer – pausiert"
                })
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => {
                        let state = app.state::<AppState>();
                        let new_val = !state.watch_state.is_enabled();
                        let _ = set_enabled(new_val, state, app.clone());
                        let _ = app.emit("enabled-changed", new_val);
                    }
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            let _tray = tray_builder.build(app)?;

            // Conditionally hide to tray or quit on window close
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                let app_h = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let state = app_h.state::<AppState>();
                        let to_tray = state.close_to_tray.load(std::sync::atomic::Ordering::Relaxed);
                        if to_tray {
                            api.prevent_close();
                            let _ = w.hide();
                        }
                    }
                });
            }

            // Load config and start WMI watcher
            let config = load_config_from_disk(app.handle());
            let startup_monitor = config.monitor_name.clone();
            let watch_state = Arc::new(WatchState::new(config));

            // Restore the persisted enabled/paused state across restarts.
            watch_state.set_enabled(app_settings.enabled);

            let tray_id_cell = std::sync::OnceLock::new();
            let _ = tray_id_cell.set(_tray.id().clone());

            app.manage(AppState {
                watch_state: Arc::clone(&watch_state),
                tray_id: tray_id_cell,
                close_to_tray: std::sync::atomic::AtomicBool::new(app_settings.close_to_tray),
            });

            // Start minimized to tray if configured
            if app_settings.start_minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }

            watcher::start(watch_state, app.handle().clone());

            // Emit startup event once the frontend is ready
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                let startup_hz = display::get_current_refresh_rate(&startup_monitor);
                let _ = app_handle.emit(
                    "hz-changed",
                    serde_json::json!({
                        "current_hz": startup_hz,
                        "hz_from": startup_hz,
                        "hz_to": startup_hz,
                        "reason": "Intelligent Hz Changer gestartet",
                        "event_type": "system"
                    }),
                );
            });

            Ok(())
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Zweite Instanz gestartet → bestehendes Fenster in den Vordergrund
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_monitors,
            get_monitors_extended,
            get_supported_hz,
            get_current_hz,
            test_hz,
            identify_monitors,
            get_process_counts,
            load_config,
            save_config,
            get_running_watched,
            get_all_running_processes,
            get_process_icon,
            set_window_theme,
            set_enabled,
            get_enabled,
            load_settings,
            save_settings,
            show_update_notification,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
