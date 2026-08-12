mod display;
mod identify;
mod logging;
mod process_icon;
mod process_watcher;
mod settings;
mod watcher;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::tray::TrayIconId;
use tauri::{Emitter, Manager, Theme, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use display::MonitorInfoExtended;
use process_watcher::{WatchConfig, WatchState, WatchedProcess};

/// Bumped by every `save_config`. The spawned apply-thread compares against it
/// after its slow reconcile so only the newest save touches the refresh rate.
static SAVE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Claims the next save generation for the calling save.
fn claim_save_generation() -> u64 {
    SAVE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// True if `generation` is still the newest claimed save. A thread whose save
/// has been superseded must not apply its now-stale Hz.
fn is_newest_save(generation: u64) -> bool {
    SAVE_GENERATION.load(Ordering::SeqCst) == generation
}

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
        // Only undo our own change: if the monitor is no longer at the rate this
        // test applied, someone else (the user in Windows display settings, or
        // another app) set it since, and restoring the pre-test value would
        // silently clobber that.
        if display::get_current_refresh_rate(&mn) != hz {
            return;
        }
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
    watched_processes: Vec<WatchedProcess>,
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

    state.watch_state.update_config(config);

    // A process just added to the list may already be running — no WMI creation
    // event will ever fire for it, so reconcile the running set once now.
    // Off the IPC thread: a full WMI process enumeration takes 100–500 ms and
    // would otherwise block the command (and with it the UI's await).
    #[cfg(windows)]
    {
        let ws = Arc::clone(&state.watch_state);
        let app_c = app.clone();
        // Two saves in quick succession spawn two threads that race for
        // `hz_lock` in arbitrary order, so the older config's Hz could win and
        // clobber the newer one. Each save claims a generation; a thread that is
        // no longer the newest bails out instead of applying a stale value.
        let generation = claim_save_generation();
        std::thread::spawn(move || {
            watcher::reconcile(&ws, &app_c);
            if !is_newest_save(generation) {
                return;
            }
            // Apply the (possibly changed) Hz values: sync_hz picks game or
            // default from the running set *after* reconcile, so editing either
            // value takes effect immediately for whichever mode is active.
            // event_type "system" — no process actually started or stopped here,
            // so this must not be counted as an automatic switch by the UI.
            if ws.is_enabled() {
                watcher::sync_hz(
                    &ws,
                    &app_c,
                    "Konfiguration gespeichert".into(),
                    None,
                    "system",
                );
            }
        });
    }
    #[cfg(not(windows))]
    if state.watch_state.is_enabled() {
        watcher::sync_hz(
            &state.watch_state,
            &app,
            "Konfiguration gespeichert".into(),
            None,
            "system",
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

/// Opens the debug log in the OS default handler. Creates it empty if it doesn't
/// exist yet so there's always something to open.
#[tauri::command]
fn open_log_file(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = logging::path().ok_or("log path not initialized")?;
    if !path.exists() {
        std::fs::write(&path, b"").map_err(|e| e.to_string())?;
    }
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
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

    // Apply the debug-logging toggle to the live runtime flag immediately.
    logging::set_enabled(s.debug_logging);

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
async fn get_process_icon(process_name: String, exe_path: Option<String>) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            // Prefer the provided path (elevated processes like Fortnite/Vanguard
            // return null ExecutablePath from WMI, so find_exe_path fails for
            // them), but only if it belongs to a real running process — the path
            // comes from the renderer and is otherwise unvalidated.
            let path = exe_path
                .filter(|p| !p.is_empty() && is_known_process_path(p))
                .or_else(|| find_exe_path(&process_name))?;
            process_icon::extract_icon_base64(&path)
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
        process_id: u32,
    }

    // spawn_blocking threads may be reused and already COM-initialized.
    let com = COMLibrary::new().unwrap_or_else(|_| unsafe { COMLibrary::assume_initialized() });
    let Ok(con) = WMIConnection::new(com) else {
        return vec![];
    };
    let results: Result<Vec<Proc>, _> = con.query();
    results
        .map(|v| {
            v.into_iter()
                .map(|p| {
                    // WMI returns null ExecutablePath for Vanguard-protected processes even as
                    // admin. QueryFullProcessImageNameW succeeds where WMI fails.
                    let path = p.executable_path.or_else(|| process_watcher::exe_path_from_pid(p.process_id));
                    (p.name, path)
                })
                .collect()
        })
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

#[derive(serde::Serialize)]
struct RunningProcess {
    name: String,
    path: Option<String>,
}

#[tauri::command]
async fn get_running_processes_with_paths() -> Vec<RunningProcess> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            let mut seen = std::collections::HashSet::new();
            let mut procs: Vec<RunningProcess> = query_processes()
                .into_iter()
                .filter(|(n, _)| !n.is_empty())
                .filter(|(n, _)| seen.insert(n.to_lowercase()))
                .map(|(name, path)| RunningProcess { name, path })
                .collect();
            procs.sort_unstable_by_key(|p| p.name.to_lowercase());
            procs
        }
        #[cfg(not(windows))]
        vec![]
    })
    .await
    .unwrap_or_default()
}

/// Cached process snapshot for icon lookups. Resolving N icons on a tab render
/// would otherwise run N full WMI enumerations (~100–500 ms each); process
/// paths barely change, so a short TTL collapses a burst into one query.
/// (process name, executable path) pairs as returned by `query_processes`.
#[cfg(windows)]
type ProcSnapshot = Vec<(String, Option<String>)>;
#[cfg(windows)]
static PROC_SNAPSHOT: std::sync::Mutex<Option<(std::time::Instant, ProcSnapshot)>> =
    std::sync::Mutex::new(None);
#[cfg(windows)]
const PROC_SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(windows)]
fn query_processes_cached() -> ProcSnapshot {
    if let Ok(guard) = PROC_SNAPSHOT.lock() {
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < PROC_SNAPSHOT_TTL {
                return cached.clone();
            }
        }
    }
    let fresh = query_processes();
    if let Ok(mut guard) = PROC_SNAPSHOT.lock() {
        *guard = Some((std::time::Instant::now(), fresh.clone()));
    }
    fresh
}

/// Resolves a process name to its executable path via WMI.
/// `ExecutablePath` is populated regardless of privilege level (unlike
/// `Get-Process .Path`, which is null for elevated processes like Vanguard).
#[cfg(windows)]
fn find_exe_path(process_name: &str) -> Option<String> {
    query_processes_cached()
        .into_iter()
        .find(|(name, path)| {
            name.eq_ignore_ascii_case(process_name)
                && path.as_deref().is_some_and(|p| !p.is_empty())
        })
        .and_then(|(_, path)| path)
}

/// True if `path` names an executable that actually belongs to a running
/// process. The renderer supplies `exe_path` for icon extraction, so this keeps
/// a compromised or buggy frontend from pointing the icon loader at an
/// arbitrary file on disk.
#[cfg(windows)]
fn is_known_process_path(path: &str) -> bool {
    query_processes_cached()
        .iter()
        .any(|(_, p)| p.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(path)))
}

/// Validates a user-picked executable path for the watch list. Restricted to
/// `.exe` files that are regular files, so this can't be used as a general
/// filesystem probe for arbitrary paths.
#[tauri::command]
fn check_exe_exists(path: String) -> bool {
    if !path.to_lowercase().ends_with(".exe") {
        return false;
    }
    std::path::Path::new(&path).is_file()
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

            // Initialize opt-in file logging before the watcher starts so the very
            // first process-start edge is captured when the toggle is on.
            if let Ok(dir) = app.path().app_config_dir() {
                let _ = std::fs::create_dir_all(&dir);
                logging::init(dir.join("debug.log"), app_settings.debug_logging);
            }

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
                        // set_enabled already emits "enabled-changed" itself.
                        let _ = set_enabled(new_val, state, app.clone());
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
            // Restore the persisted enabled/paused state across restarts up
            // front, so the watcher never sees a stale `true`.
            let watch_state = Arc::new(WatchState::new(config, app_settings.enabled));

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
            get_running_processes_with_paths,
            get_process_icon,
            set_window_theme,
            set_enabled,
            get_enabled,
            load_settings,
            save_settings,
            open_log_file,
            show_update_notification,
            check_exe_exists,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serialized by the shared counter: these must not run concurrently, and
    // cargo test does run them on separate threads. Each claims its own
    // generations, so ordering between tests is irrelevant — only the relative
    // order of claims within a test matters.
    #[test]
    fn only_the_newest_save_applies_hz() {
        // A lone save is the newest and applies.
        let first = claim_save_generation();
        assert!(is_newest_save(first));

        // A second save lands while the first thread is still reconciling. The
        // stale thread must bail; the newest one applies.
        let second = claim_save_generation();
        assert!(!is_newest_save(first));
        assert!(is_newest_save(second));

        // Generations are strictly increasing, so a late claim never collides
        // with an earlier one still in flight.
        assert!(second > first);
    }
}
