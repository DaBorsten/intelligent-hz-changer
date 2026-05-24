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

#[tauri::command]
fn test_hz(monitor_name: String, hz: u32) -> Result<(), String> {
    let current = display::get_current_refresh_rate(&monitor_name);
    display::set_refresh_rate(&monitor_name, hz)?;
    let mn = monitor_name.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
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
        let cfg = state.watch_state.config.lock().unwrap();
        let monitor = cfg.monitor_name.clone();
        let hz = cfg.default_hz_for(&monitor);
        drop(cfg);
        let prev_hz = crate::display::get_current_refresh_rate(&monitor);
        if let Err(e) = crate::display::set_refresh_rate(&monitor, hz) {
            eprintln!("set_refresh_rate error: {e}");
        }
        let _ = app.emit(
            "hz-changed",
            serde_json::json!({
                "current_hz": hz,
                "hz_from": prev_hz,
                "hz_to": hz,
                "reason": "Prozess aus Liste entfernt (kein aktiver Prozess mehr)",
                "event_type": "process_stop"
            }),
        );
    }
    Ok(())
}

#[tauri::command]
fn set_enabled(
    value: bool,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    state.watch_state.set_enabled(value);

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
    s: settings::AppSettings,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
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
        let exe_path = find_exe_path(&process_name)?;
        process_icon::extract_icon_base64(&exe_path)
    })
    .await
    .unwrap_or(None)
}

#[tauri::command]
async fn get_all_running_processes() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(|| {
        use std::process::Command;
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-Process | Where-Object { $_.Name -ne '' } | Select-Object -ExpandProperty Name -Unique | Sort-Object",
            ])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output();
        let Ok(out) = output else { return vec![] };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| format!("{}.exe", l.trim()))
            .filter(|s| s.len() > 4)
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn find_exe_path(process_name: &str) -> Option<String> {
    use std::process::Command;
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    // Get-Process .Path returns null for elevated processes (e.g. Valorant/Vanguard).
    // WMI ExecutablePath works regardless of privilege level.
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(Get-CimInstance Win32_Process -Filter \"Name = '{}'\" | Select-Object -First 1).ExecutablePath",
                process_name
            ),
        ])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
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

            // System tray
            let toggle_item = MenuItem::with_id(app, "toggle", "Deaktivieren", true, None::<&str>)?;
            let sep_item = PredefinedMenuItem::separator(app)?;
            let open_item =
                MenuItem::with_id(app, "open", "App öffnen", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle_item, &sep_item, &open_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Intelligent Hz Changer – aktiv")
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
                })
                .build(app)?;

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

            let app_settings: settings::AppSettings = app.path().app_config_dir()
                .ok()
                .and_then(|dir| std::fs::read_to_string(dir.join("settings.json")).ok())
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default();

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
