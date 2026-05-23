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

/// Polls Win32_Process every 2 s — no admin required.
/// Spawns a watch_exit thread for each new watched process detected.
#[cfg(windows)]
fn run_start_watcher(state: Arc<WatchState>, app: tauri::AppHandle) {
    use serde::Deserialize;
    use wmi::{COMLibrary, WMIConnection};

    #[derive(Deserialize)]
    #[serde(rename = "Win32_Process")]
    struct ProcessEntry {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "ProcessId")]
        process_id: u32,
    }

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

    // PIDs we have already spawned a watch_exit thread for, still alive.
    let mut watching: HashSet<u32> = HashSet::new();

    loop {
        std::thread::sleep(Duration::from_secs(2));

        let processes: Vec<ProcessEntry> = match wmi_con.query() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("WMI query error: {e}");
                continue;
            }
        };

        let current_pids: HashSet<u32> = processes.iter().map(|p| p.process_id).collect();

        // Config was updated — clear watching so already-running processes are re-evaluated.
        if state.needs_rescan.swap(false, std::sync::atomic::Ordering::Relaxed) {
            watching.clear();
        }

        // Check for newly started watched processes.
        for proc in &processes {
            if watching.contains(&proc.process_id) {
                continue;
            }
            let is_watched = {
                let cfg = state.config.lock().unwrap();
                cfg.watched_processes
                    .iter()
                    .any(|w| w.to_lowercase() == proc.name.to_lowercase())
            };
            if !is_watched {
                continue;
            }

            watching.insert(proc.process_id);

            if state.on_process_start(&proc.name) && state.is_enabled() {
                let cfg = state.config.lock().unwrap();
                let monitor = cfg.monitor_name.clone();
                let hz = cfg.game_hz_for(&monitor);
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
                        "reason": format!("{} gestartet", proc.name),
                        "process_name": proc.name,
                        "event_type": "process_start"
                    }),
                );
            }

            let state_c = Arc::clone(&state);
            let app_c = app.clone();
            let name_c = proc.name.clone();
            let pid = proc.process_id;
            std::thread::spawn(move || watch_exit(pid, name_c, state_c, app_c));
        }

        // Drop PIDs of processes that are gone so we detect restarts.
        watching.retain(|pid| current_pids.contains(pid));
    }
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

    if state.on_process_stop(&name) && state.is_enabled() {
        let cfg = state.config.lock().unwrap();
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
                "reason": format!("{} beendet", name),
                "process_name": name,
                "event_type": "process_stop"
            }),
        );
    }
}
