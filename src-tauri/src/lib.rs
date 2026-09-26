mod display;
mod identify;
#[cfg(target_os = "linux")]
mod linux_theme;
mod logging;
mod process_icon;
mod process_watcher;
mod settings;
mod watcher;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::tray::TrayIconId;
use tauri::{Emitter, Manager, RunEvent, Theme};
use tauri_plugin_notification::NotificationExt;

use display::MonitorInfoExtended;
use process_watcher::{WatchConfig, WatchState};

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

/// Applies the theme the user picked (`light`, `dark`, or `system`) to the
/// native window and reports back which one that resolved to, so the
/// frontend can style the webview to match. `None` means "couldn't tell" —
/// the frontend then falls back to its own `prefers-color-scheme` query.
#[tauri::command]
fn set_window_theme(app: tauri::AppHandle, theme: String) -> Result<Option<String>, String> {
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let resolved = match theme.as_str() {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => {
            #[cfg(target_os = "linux")]
            {
                linux_theme::system_theme()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        }
    };

    // Passing `None` here means "follow the OS", which is what we want on
    // Windows/macOS in system mode. On Linux it instead resets
    // `gtk-application-prefer-dark-theme` to false — a light title bar on a
    // dark desktop — which is why `system_theme` resolves it beforehand.
    window.set_theme(resolved).map_err(|e| e.to_string())?;

    #[cfg(target_os = "linux")]
    linux_theme::sync_gtk_theme(&app, resolved == Some(Theme::Dark));

    Ok(match resolved {
        Some(Theme::Dark) => Some("dark".to_string()),
        Some(Theme::Light) => Some("light".to_string()),
        _ => None,
    })
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

/// Reports whether the platform's display backend is usable, so an empty
/// monitor list can be explained rather than just shown.
#[tauri::command]
fn get_display_backend_status() -> display::BackendStatus {
    display::backend_status()
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
/// value once their countdown ran out, clobbering one another.
static TEST_HZ_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long the frontend should show a test as running — and label the button
/// with. Not a constant: GNOME runs the trial through its own confirmation
/// prompt on a timeout we don't control.
#[tauri::command]
fn get_test_seconds() -> u32 {
    display::test_seconds()
}

#[tauri::command]
fn test_hz(
    monitor_name: String,
    hz: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let ws = Arc::clone(&state.watch_state);
    let (current, desktop_owns_trial) = {
        let _guard = ws.hz_lock.lock().unwrap_or_else(|e| e.into_inner());
        let current = display::get_current_refresh_rate(&monitor_name);
        let owned = display::set_refresh_rate_for_test(&monitor_name, hz)?;
        (current, owned)
    };
    // Bump the token even when we don't revert ourselves, so a still-pending
    // revert from an earlier test can't fire on top of this one.
    let token = TEST_HZ_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if desktop_owns_trial {
        return Ok(());
    }
    let mn = monitor_name.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(
            display::test_seconds() as u64
        ));
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
fn identify_monitors(app: tauri::AppHandle, theme: Option<String>) {
    let monitors = display::get_monitors_extended();
    let is_light = theme.as_deref() == Some("light");
    identify::show_overlays(&app, monitors, is_light);
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
        WatchConfig::parse(&json).map_err(|e| e.to_string())
    } else {
        Ok(WatchConfig::default())
    }
}

#[tauri::command]
fn save_config(
    config: WatchConfig,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("config.json"), json).map_err(|e| e.to_string())?;

    state.watch_state.update_config(config);

    // A process just added to the list may already be running — no WMI creation
    // event (Windows) or poll tick (Linux) will otherwise catch it for up to
    // one interval, so reconcile the running set once now. Off the IPC thread:
    // a full process enumeration takes 100–500 ms and would otherwise block
    // the command (and with it the UI's await).
    #[cfg(any(windows, target_os = "linux"))]
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
    #[cfg(not(any(windows, target_os = "linux")))]
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
        let defaults = state
            .watch_state
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hz_targets(&[]);
        watcher::apply_targets(
            &state.watch_state,
            &app,
            &defaults,
            "Pausiert".into(),
            None,
            "system",
        );
    }

    // Update tray tooltip + menu item label
    if let Some(tray_id) = state.tray_id.get() {
        if let Some(tray) = app.tray_by_id(tray_id) {
            // Tray tooltips are unsupported on Linux (see the tray setup in
            // `run`), so the menu label is the only state indicator there.
            #[cfg(not(target_os = "linux"))]
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
fn get_hz_log() -> Vec<serde_json::Value> {
    watcher::hz_log()
}

/// Shows the main window, creating it first if it doesn't exist. The window is
/// destroyed rather than hidden while the app sits in the tray, so its webview
/// (by far the largest share of the app's memory) isn't kept alive for nothing.
fn show_main_window(app: &tauri::AppHandle) {
    // Still loading: it reveals itself once the page is ready, and showing it
    // now would put the blank webview on screen.
    if MAIN_WINDOW_LOADING.load(Ordering::Acquire) {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // Building a webview from a menu/tray handler deadlocks on Windows
    // (WebView2 needs the event loop those handlers run on), so do it off-thread.
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = create_main_window(&app);
    });
}

/// Set from the moment the main window is being built until its page has
/// loaded. Keeps a second tray click or instance launch from building it twice
/// or revealing it half-loaded.
static MAIN_WINDOW_LOADING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn create_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main") else {
        return Ok(());
    };
    if MAIN_WINDOW_LOADING.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let res = build_main_window(app, config);
    if res.is_err() {
        MAIN_WINDOW_LOADING.store(false, Ordering::Release);
    }
    res
}

fn build_main_window(app: &tauri::AppHandle, config: &tauri::utils::config::WindowConfig) -> tauri::Result<()> {
    use tauri::webview::PageLoadEvent;
    use tauri::window::Color;

    // A fresh webview paints white until the page's own background lands. Match
    // the page background (index.css `body`) up front so dark mode never flashes.
    let dark = prefers_dark(app);
    let (theme, background) = if dark {
        (Theme::Dark, Color(0x14, 0x14, 0x14, 0xff))
    } else {
        (Theme::Light, Color(0xf0, 0xee, 0xeb, 0xff))
    };
    tauri::WebviewWindowBuilder::from_config(app, config)?
        .theme(Some(theme))
        .background_color(background)
        // The window is created hidden (`visible: false`); reveal it once the
        // page has loaded, so its first visible frame is the rendered UI.
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                MAIN_WINDOW_LOADING.store(false, Ordering::Release);
                let _ = window.show();
                let _ = window.set_focus();
            }
        })
        .build()?;
    Ok(())
}

/// Whether the window should open dark: the user's theme setting, with
/// "system" resolved against the OS preference.
fn prefers_dark(app: &tauri::AppHandle) -> bool {
    match read_settings(app).theme.as_str() {
        "dark" => true,
        "light" => false,
        _ => {
            #[cfg(windows)]
            {
                windows_prefers_dark()
            }
            #[cfg(target_os = "linux")]
            {
                linux_theme::system_theme() == Some(Theme::Dark)
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            {
                false
            }
        }
    }
}

/// Reads the "app mode" from Settings → Personalization → Colors.
#[cfg(windows)]
fn windows_prefers_dark() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let res = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    res.is_ok() && value == 0
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

    // Always reflect the real autostart state (registry on Windows, the XDG
    // .desktop file elsewhere), not the stored value. This way external
    // changes (e.g. Task Manager or the GNOME "Automatisch ausführen"
    // toggle) are shown correctly.
    s.autostart = settings::get_autostart("IntelligentHzChanger");

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

    // Autostart via Windows registry, or an XDG .desktop file elsewhere.
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    settings::set_autostart("IntelligentHzChanger", &exe, s.autostart)?;

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
        #[cfg(target_os = "linux")]
        {
            // No validation of `exe_path` needed here (unlike Windows): the
            // lookup never opens it, it only compares its basename against
            // desktop entries, and the icon file itself comes from those.
            process_icon::lookup_icon_base64(&process_name, exe_path.as_deref())
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (process_name, exe_path);
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
        #[cfg(target_os = "linux")]
        {
            let mut names: Vec<String> = list_user_processes_linux()
                .into_iter()
                .map(|p| p.name)
                .collect();
            names.sort_unstable_by_key(|n| n.to_lowercase());
            names.dedup_by_key(|n| n.to_lowercase());
            names
        }
        #[cfg(not(any(windows, target_os = "linux")))]
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
        #[cfg(target_os = "linux")]
        {
            let mut seen = std::collections::HashSet::new();
            let mut procs: Vec<RunningProcess> = list_user_processes_linux()
                .into_iter()
                .filter(|p| seen.insert(p.name.to_lowercase()))
                .map(|p| RunningProcess { name: p.name, path: Some(p.exe_path) })
                .collect();
            procs.sort_unstable_by_key(|p| p.name.to_lowercase());
            procs
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        vec![]
    })
    .await
    .unwrap_or_default()
}

/// Running processes worth offering in the picker. `/proc` also lists kernel
/// threads and other users' daemons, whose `exe` symlink can't be resolved —
/// dropping those leaves the user's own applications, which is all a watch list
/// can meaningfully contain.
#[cfg(target_os = "linux")]
fn list_user_processes_linux() -> Vec<watcher::ProcInfo> {
    watcher::list_processes_linux()
        .into_iter()
        .filter(|p| !p.exe_path.is_empty() && !p.name.is_empty())
        .collect()
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
/// executables — `.exe` files on Windows, files carrying an execute bit on
/// Linux (which has no extension to go by) — so this can't be used as a general
/// filesystem probe for arbitrary paths.
#[tauri::command]
fn check_exe_exists(path: String) -> bool {
    #[cfg(windows)]
    {
        if !path.to_lowercase().ends_with(".exe") {
            return false;
        }
        std::path::Path::new(&path).is_file()
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(any(windows, unix)))]
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
        WatchConfig::parse(&json).unwrap_or_default()
    } else {
        WatchConfig::default()
    }
}

/// Handles the case where this process is the identify-overlay helper that
/// `identify` spawned, rather than the app itself: draws the badges, and
/// returns `true` so `main` exits without starting Tauri — in particular
/// before the single-instance plugin would see a second instance.
#[cfg(target_os = "linux")]
pub fn run_overlay_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(identify::OVERLAY_ARG) {
        return false;
    }
    if let Some(payload) = args.next() {
        identify::run_overlay_process(&payload);
    }
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::TrayIconBuilder;
            #[cfg(not(target_os = "linux"))]
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

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
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => {
                        let state = app.state::<AppState>();
                        let new_val = !state.watch_state.is_enabled();
                        // set_enabled already emits "enabled-changed" itself.
                        let _ = set_enabled(new_val, state, app.clone());
                    }
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                });

            // Tooltips and icon clicks only exist on Windows/macOS. The Linux
            // tray is an AppIndicator: `set_tooltip` is a documented no-op and
            // `TrayIconEvent` is never emitted, so neither is set up there —
            // the context menu ("App öffnen") is the whole interaction.
            #[cfg(not(target_os = "linux"))]
            {
                tray_builder = tray_builder
                    // Left click opens the window; the menu is right-click only.
                    .show_menu_on_left_click(false)
                    .tooltip(if app_settings.enabled {
                        "Intelligent Hz Changer – aktiv"
                    } else {
                        "Intelligent Hz Changer – pausiert"
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main_window(tray.app_handle());
                        }
                    });
            }
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            let _tray = tray_builder.build(app)?;

            // Load config and start WMI watcher
            let config = load_config_from_disk(app.handle());
            let startup_monitor = config.status_monitor().unwrap_or_default().to_string();
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

            // The window isn't created from the config (`create: false`), so
            // starting minimized never spins up a webview at all.
            if !app_settings.start_minimized {
                create_main_window(app.handle())?;
            }

            watcher::start(watch_state, app.handle().clone());

            // Emit startup event once the frontend is ready
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                let startup_hz = display::get_current_refresh_rate(&startup_monitor);
                watcher::emit_hz_changed(
                    &app_handle,
                    serde_json::json!({
                        "current_hz": startup_hz,
                        "hz_from": startup_hz,
                        "hz_to": startup_hz,
                        "reason": "Intelligent Hz Changer gestartet",
                        "event_type": "system",
                        "monitor": startup_monitor,
                    }),
                );
            });

            Ok(())
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Zweite Instanz gestartet → Fenster (ggf. neu erzeugen) in den Vordergrund
            show_main_window(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_monitors,
            get_monitors_extended,
            get_display_backend_status,
            get_supported_hz,
            get_current_hz,
            test_hz,
            get_test_seconds,
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
            get_hz_log,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            // Closing the last window asks to exit (`code: None`). With
            // close-to-tray on, the window is simply gone — freeing its webview —
            // while the watcher and tray keep running. Tray "Beenden" exits with
            // an explicit code and is never held back.
            if let RunEvent::ExitRequested { code: None, api, .. } = event {
                let state = app.state::<AppState>();
                if state.close_to_tray.load(std::sync::atomic::Ordering::Relaxed) {
                    api.prevent_exit();
                }
            }
        });
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
