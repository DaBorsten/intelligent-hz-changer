use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

use crate::process_watcher::WatchState;

pub fn start(state: Arc<WatchState>, app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        run_start_watcher(state, app_handle);
        #[cfg(not(windows))]
        let _ = (state, app_handle);
    });
}

#[cfg(windows)]
use serde::Deserialize;

/// A Win32_Process row (also the `TargetInstance` of a creation event).
#[cfg(windows)]
#[derive(Deserialize)]
#[serde(rename = "Win32_Process")]
struct ProcessEntry {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ProcessId")]
    process_id: u32,
}

/// `__InstanceCreationEvent` carrying the newly created `Win32_Process`.
#[cfg(windows)]
#[derive(Deserialize)]
#[serde(rename = "__InstanceCreationEvent")]
struct NewProcessEvent {
    #[serde(rename = "TargetInstance")]
    target_instance: ProcessEntry,
}

/// Event-driven start detection — no admin required.
///
/// Subscribes to WMI `__InstanceCreationEvent` for `Win32_Process` instead of
/// enumerating every process on a timer: our thread blocks until a process is
/// actually created and only receives the new instance. (WMI still polls at the
/// `WITHIN` interval internally — truly poll-free start detection needs the ETW
/// kernel provider, which requires admin.) Falls back to timer polling if the
/// subscription can't be established (e.g. locked-down WMI).
#[cfg(windows)]
fn run_start_watcher(state: Arc<WatchState>, app: tauri::AppHandle) {
    use wmi::{COMLibrary, WMIConnection};

    let com_lib = match COMLibrary::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("WMI COM init failed: {e}");
            return;
        }
    };
    let wmi_con = match WMIConnection::new(com_lib) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("WMI connect failed: {e}");
            return;
        }
    };

    if let Err(e) = run_event_loop(&wmi_con, &state, &app) {
        eprintln!("WMI event subscription failed ({e}); falling back to polling");
        run_poll_loop(&wmi_con, &state, &app);
    }
}

/// Blocks on the creation-event stream forever. Returns `Err` only if the
/// subscription itself can't be set up, so the caller can fall back to polling.
#[cfg(windows)]
fn run_event_loop(
    wmi_con: &wmi::WMIConnection,
    state: &Arc<WatchState>,
    app: &tauri::AppHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use wmi::FilterValue;

    let mut filters = HashMap::new();
    filters.insert("TargetInstance".to_owned(), FilterValue::is_a::<ProcessEntry>()?);

    // Activating the subscription before the initial scan means any process that
    // starts during the scan is queued and handled right after (dedup prevents
    // double handling).
    let iterator =
        wmi_con.filtered_notification::<NewProcessEvent>(&filters, Some(Duration::from_secs(1)))?;

    // Events only fire for processes started *after* subscribing — catch those
    // already running now.
    reconcile(state, app);

    for event in iterator {
        match event {
            Ok(ev) => register_process(state, app, &ev.target_instance.name, ev.target_instance.process_id),
            Err(e) => eprintln!("WMI notification error: {e}"),
        }
    }
    Ok(())
}

/// Timer fallback used only when the event subscription is unavailable.
#[cfg(windows)]
fn run_poll_loop(wmi_con: &wmi::WMIConnection, state: &Arc<WatchState>, app: &tauri::AppHandle) {
    loop {
        let processes: Vec<ProcessEntry> = match wmi_con.query() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("WMI query error: {e}");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        let current_pids: HashSet<u32> = processes.iter().map(|p| p.process_id).collect();
        // Drop dead PIDs so a watch_exit thread is re-armed on restart.
        state
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|pid| current_pids.contains(pid));

        let watched: HashSet<String> = {
            let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.watched_processes.iter().map(|w| w.to_lowercase()).collect()
        };
        for proc in &processes {
            if watched.contains(&proc.name.to_lowercase()) {
                register_process(state, app, &proc.name, proc.process_id);
            }
        }

        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Enumerates running processes once and registers every watched one. Used for
/// the initial scan and after a config change (a process may already be running
/// when it is added to the watch list — no creation event will ever fire for it).
#[cfg(windows)]
pub fn reconcile(state: &Arc<WatchState>, app: &tauri::AppHandle) {
    use wmi::{COMLibrary, WMIConnection};

    // Own short-lived connection so we never enumerate on the notification
    // connection. spawn_blocking/command threads may already be COM-initialized.
    let com = COMLibrary::new().unwrap_or_else(|_| unsafe { COMLibrary::assume_initialized() });
    let Ok(con) = WMIConnection::new(com) else {
        return;
    };
    let Ok(processes): Result<Vec<ProcessEntry>, _> = con.query() else {
        return;
    };

    let watched: HashSet<String> = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.watched_processes.iter().map(|w| w.to_lowercase()).collect()
    };
    for proc in &processes {
        if watched.contains(&proc.name.to_lowercase()) {
            register_process(state, app, &proc.name, proc.process_id);
        }
    }
}

/// Registers one watched process: triggers Hz up on the empty→non-empty edge and
/// arms exactly one `watch_exit` thread for the PID. Safe to call repeatedly —
/// `on_process_start` is idempotent per PID and the spawn is gated on the shared
/// `watching` set, so neither counts nor threads are duplicated.
#[cfg(windows)]
fn register_process(state: &Arc<WatchState>, app: &tauri::AppHandle, name: &str, pid: u32) {
    if state.on_process_start(name, pid) && state.is_enabled() {
        sync_hz(state, app, format!("{name} gestartet"), Some(name.to_string()), "process_start");
    }

    let newly = state
        .watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pid);
    if newly {
        let state_c = Arc::clone(state);
        let app_c = app.clone();
        let name_c = name.to_string();
        std::thread::spawn(move || watch_exit(pid, name_c, state_c, app_c));
    }
}

/// Applies `target` Hz to `monitor` under the global `hz_lock` and emits the
/// event. Serializing every transition through one lock guarantees concurrent
/// up/down transitions can't land out of order; a no-op (already at target) is
/// skipped so no spurious mode-set or event is produced.
pub fn set_monitor_hz(
    state: &WatchState,
    app: &tauri::AppHandle,
    monitor: &str,
    target: u32,
    reason: String,
    process_name: Option<String>,
    event_type: &str,
) {
    let _guard = state.hz_lock.lock().unwrap_or_else(|e| e.into_inner());
    let prev = crate::display::get_current_refresh_rate(monitor);
    if prev == target {
        return;
    }
    if let Err(e) = crate::display::set_refresh_rate(monitor, target) {
        eprintln!("set_refresh_rate error: {e}");
        return;
    }
    let mut payload = serde_json::json!({
        "current_hz": target,
        "hz_from": prev,
        "hz_to": target,
        "reason": reason,
        "event_type": event_type,
    });
    if let Some(name) = process_name {
        payload["process_name"] = serde_json::json!(name);
    }
    let _ = app.emit("hz-changed", payload);
}

/// Computes the correct target Hz from the *current* running set and config,
/// then applies it. Whoever runs last during a race wins with the right value.
pub fn sync_hz(
    state: &WatchState,
    app: &tauri::AppHandle,
    reason: String,
    process_name: Option<String>,
    event_type: &str,
) {
    let any_running = state.is_any_running();
    let (monitor, target) = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let monitor = cfg.monitor_name.clone();
        let target = if any_running {
            cfg.game_hz_for(&monitor)
        } else {
            cfg.default_hz_for(&monitor)
        };
        (monitor, target)
    };
    set_monitor_hz(state, app, &monitor, target, reason, process_name, event_type);
}

/// Blocks until the process exits using a kernel event — zero CPU overhead.
/// No admin required: PROCESS_SYNCHRONIZE works on all user processes.
#[cfg(windows)]
fn watch_exit(pid: u32, name: String, state: Arc<WatchState>, app: tauri::AppHandle) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => {
            unsafe { WaitForSingleObject(handle, u32::MAX) }; // INFINITE
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
        Err(_) => {
            // Process already terminated before we could open it.
        }
    }

    // Release the PID so a restart re-arms a fresh watch_exit thread.
    state
        .watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&pid);

    if state.on_process_stop(pid) && state.is_enabled() {
        sync_hz(
            &state,
            &app,
            format!("{} beendet", name),
            Some(name),
            "process_stop",
        );
    }
}
