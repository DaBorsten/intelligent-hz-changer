use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

use crate::process_watcher::WatchState;

pub fn start(state: Arc<WatchState>, app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        run_start_watcher(state, app_handle);
        #[cfg(target_os = "linux")]
        run_start_watcher_linux(state, app_handle);
        #[cfg(not(any(windows, target_os = "linux")))]
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
    #[serde(rename = "ExecutablePath")]
    executable_path: Option<String>,
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
            Ok(ev) => {
                let pid = ev.target_instance.process_id;
                let exe = ev.target_instance.executable_path
                    .filter(|p| !p.is_empty())
                    .or_else(|| crate::process_watcher::exe_path_from_pid(pid))
                    .unwrap_or_default();
                register_process(state, app, &ev.target_instance.name, &exe, pid);
            }
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

        // Retire watched PIDs that vanished. watch_exit normally owns this edge,
        // but it can't observe a process it never got a handle for (exited too
        // fast, or OpenProcess denied), which would otherwise pin `running`
        // non-empty forever and strand the monitor at game Hz.
        let dead: Vec<u32> = state.running_pids().into_iter()
            .filter(|pid| !current_pids.contains(pid))
            .collect();
        for pid in dead {
            if state.on_process_stop(pid) && state.is_enabled() {
                sync_hz(state, app, "Prozess beendet".into(), None, "process_stop");
            }
        }

        let watched: Vec<crate::process_watcher::WatchedProcess> = {
            let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.watched_processes.clone()
        };
        for proc in &processes {
            let exe = proc.executable_path.clone()
                .filter(|p| !p.is_empty())
                .or_else(|| crate::process_watcher::exe_path_from_pid(proc.process_id))
                .unwrap_or_default();
            if watched.iter().any(|w| w.matches(&proc.name, &exe)) {
                register_process(state, app, &proc.name, &exe, proc.process_id);
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

    let watched: Vec<crate::process_watcher::WatchedProcess> = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.watched_processes.clone()
    };
    for proc in &processes {
        let exe = proc.executable_path.clone()
            .filter(|p| !p.is_empty())
            .or_else(|| crate::process_watcher::exe_path_from_pid(proc.process_id))
            .unwrap_or_default();
        if watched.iter().any(|w| w.matches(&proc.name, &exe)) {
            register_process(state, app, &proc.name, &exe, proc.process_id);
        }
    }
}

/// One running process, as read from `/proc`.
#[cfg(target_os = "linux")]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
}

/// Linux has no process-creation event API available without admin/root
/// (netlink proc connector) or a kernel new enough to guarantee pidfd-based
/// polling, so this scans `/proc` directly — the same tradeoff the Windows
/// WMI poll fallback already makes.
#[cfg(target_os = "linux")]
pub fn list_processes_linux() -> Vec<ProcInfo> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return result;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let exe_path = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = linux_process_name(pid, &exe_path);
        if name.is_empty() {
            continue;
        }
        result.push(ProcInfo { pid, name, exe_path });
    }
    result
}

/// `/proc/<pid>/comm` mirrors what `ps`/`top` show, and — notably — is what
/// the kernel sets for shebang-launched scripts (the script's own basename,
/// not the interpreter). The `/proc/<pid>/exe` symlink target would instead
/// resolve to e.g. `/usr/bin/bash` for a script, or to a Wine/Proton loader
/// binary rather than the wrapped Windows .exe's own name, so `comm` matches
/// what a user configuring the watch list actually expects to type in.
///
/// The kernel truncates `comm` to 15 bytes, which would otherwise make a
/// longer-named binary impossible to match against a watch entry the user typed
/// out in full. So when `comm` is exactly that long and the executable's own
/// basename extends it, that basename is the untruncated name and wins. Script
/// and Wine/Proton launches keep `comm`: their `exe` symlink resolves to the
/// interpreter/loader, whose basename does not extend `comm`, so the check
/// simply doesn't fire.
#[cfg(target_os = "linux")]
fn linux_process_name(pid: u32, exe_path: &str) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if comm.len() == 15 {
        if let Some(base) = std::path::Path::new(exe_path)
            .file_name()
            .map(|b| b.to_string_lossy().into_owned())
        {
            if base.len() > comm.len() && base.starts_with(&comm) {
                return base;
            }
        }
    }
    comm
}

#[cfg(target_os = "linux")]
pub fn reconcile(state: &Arc<WatchState>, app: &tauri::AppHandle) {
    let watched: Vec<crate::process_watcher::WatchedProcess> = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.watched_processes.clone()
    };
    for proc in &list_processes_linux() {
        if watched.iter().any(|w| w.matches(&proc.name, &proc.exe_path)) {
            register_process(state, app, &proc.name, &proc.exe_path, proc.pid);
        }
    }
}

/// Polls `/proc` every 2s: registers newly-matched watched processes and
/// retires PIDs that vanished (mirrors `run_poll_loop`'s dead-PID handling —
/// `watch_exit` normally owns that edge but can't observe a process it never
/// got a handle for).
#[cfg(target_os = "linux")]
fn run_start_watcher_linux(state: Arc<WatchState>, app: tauri::AppHandle) {
    loop {
        let processes = list_processes_linux();
        let current_pids: HashSet<u32> = processes.iter().map(|p| p.pid).collect();

        state
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|pid| current_pids.contains(pid));

        let dead: Vec<u32> = state
            .running_pids()
            .into_iter()
            .filter(|pid| !current_pids.contains(pid))
            .collect();
        for pid in dead {
            if state.on_process_stop(pid) && state.is_enabled() {
                sync_hz(&state, &app, "Prozess beendet".into(), None, "process_stop");
            }
        }

        let watched: Vec<crate::process_watcher::WatchedProcess> = {
            let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.watched_processes.clone()
        };
        for proc in &processes {
            if watched.iter().any(|w| w.matches(&proc.name, &proc.exe_path)) {
                register_process(&state, &app, &proc.name, &proc.exe_path, proc.pid);
            }
        }

        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Registers one watched process: triggers Hz up on the empty→non-empty edge and
/// arms exactly one `watch_exit` thread for the PID. Safe to call repeatedly —
/// `on_process_start` is idempotent per PID and the spawn is gated on the shared
/// `watching` set, so neither counts nor threads are duplicated.
#[cfg(any(windows, target_os = "linux"))]
fn register_process(state: &Arc<WatchState>, app: &tauri::AppHandle, name: &str, exe_path: &str, pid: u32) {
    let started = state.on_process_start(name, exe_path, pid);
    if started {
        crate::logging::log(&format!(
            "process_start edge: name='{name}' pid={pid} exe='{exe_path}' enabled={}",
            state.is_enabled()
        ));
    }
    if started && state.is_enabled() {
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
    crate::logging::log(&format!(
        "set_monitor_hz: monitor='{monitor}' prev={prev}Hz target={target}Hz reason='{reason}' event={event_type}"
    ));
    if prev == target {
        crate::logging::log("  -> skip: monitor already at target (no-op)");
        return;
    }
    if let Err(e) = crate::display::set_refresh_rate(monitor, target) {
        crate::logging::log(&format!("  -> set_refresh_rate FAILED: {e}"));
        eprintln!("set_refresh_rate error: {e}");
        // Tell the UI. A silent failure looks identical to a switch that never
        // needed to happen, and the displayed Hz then simply disagrees with the
        // mode the user configured until they go looking for the log.
        let _ = app.emit(
            "hz-error",
            serde_json::json!({
                "monitor": monitor,
                "target_hz": target,
                "reason": reason,
                "error": e,
            }),
        );
        return;
    }
    let after = crate::display::get_current_refresh_rate(monitor);
    crate::logging::log(&format!(
        "  -> set_refresh_rate OK: {prev}Hz -> {target}Hz (monitor now reports {after}Hz)"
    ));
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
    crate::logging::log(&format!(
        "sync_hz: any_running={any_running} -> target={target}Hz (event={event_type}, reason='{reason}')"
    ));
    set_monitor_hz(state, app, &monitor, target, reason, process_name, event_type);
}

/// Shared tail once a watched process's exit has been detected: release the
/// PID (so a restart re-arms a fresh `watch_exit` thread) and fire the
/// empty-set edge if this was the last running instance.
#[cfg(any(windows, target_os = "linux"))]
fn on_watched_process_exited(pid: u32, name: String, state: Arc<WatchState>, app: tauri::AppHandle) {
    state
        .watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&pid);

    let stopped_last = state.on_process_stop(pid);
    if stopped_last {
        crate::logging::log(&format!("process_stop edge: name='{name}' pid={pid} (last instance)"));
    }
    if stopped_last && state.is_enabled() {
        sync_hz(
            &state,
            &app,
            format!("{} beendet", name),
            Some(name),
            "process_stop",
        );
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

    on_watched_process_exited(pid, name, state, app);
}

/// Blocks until the process exits by polling for `/proc/<pid>` to disappear.
/// Linux has no portable "wait for an arbitrary PID" primitive without pidfd
/// (kernel 5.3+/glibc 2.36+, not guaranteed on every distro) or the root-only
/// netlink proc connector, so this mirrors the WMI poll-loop fallback already
/// used on Windows. A 1s interval is negligible for a handful of watched PIDs.
#[cfg(target_os = "linux")]
fn watch_exit(pid: u32, name: String, state: Arc<WatchState>, app: tauri::AppHandle) {
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        std::thread::sleep(Duration::from_millis(1000));
    }

    on_watched_process_exited(pid, name, state, app);
}
