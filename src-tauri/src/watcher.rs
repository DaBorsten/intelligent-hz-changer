use std::collections::{HashSet, VecDeque};
#[cfg(windows)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Emitter;

use crate::process_watcher::WatchState;

/// How many `hz-changed` events the backend remembers for the status view.
const HZ_LOG_CAP: usize = 50;

/// Recent `hz-changed` payloads, oldest first. Kept here rather than in the
/// webview because the window is destroyed while the app sits in the tray, so
/// a freshly created window can still show what happened in the meantime.
static HZ_LOG: Mutex<VecDeque<serde_json::Value>> = Mutex::new(VecDeque::new());

/// Source of `hz-changed` ids. Timestamps can't identify an event: two can land
/// in the same millisecond (a stop edge next to the startup ping, say).
static NEXT_HZ_ID: AtomicU64 = AtomicU64::new(1);

/// Stamps `payload` with an id and the current time, records it, and emits
/// `hz-changed`.
pub fn emit_hz_changed(app: &tauri::AppHandle, mut payload: serde_json::Value) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    payload["time"] = serde_json::json!(now_ms);
    {
        let mut log = HZ_LOG.lock().unwrap_or_else(|e| e.into_inner());
        // Taken under the lock so ids follow the log's order.
        payload["id"] = serde_json::json!(NEXT_HZ_ID.fetch_add(1, Ordering::Relaxed));
        if log.len() == HZ_LOG_CAP {
            log.pop_front();
        }
        log.push_back(payload.clone());
    }
    let _ = app.emit("hz-changed", payload);
}

/// Snapshot of the recorded `hz-changed` events, oldest first.
pub fn hz_log() -> Vec<serde_json::Value> {
    HZ_LOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

pub fn start(state: Arc<WatchState>, app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        #[cfg(any(windows, target_os = "linux"))]
        run_start_watcher(state, app_handle);
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = (state, app_handle);
    });
}

/// Serializes everything that derives start/stop edges from a process
/// snapshot: the periodic scan, the on-demand `reconcile`, and the exit
/// handler. Without it, a scan working from a snapshot taken a moment earlier
/// could retire a process `reconcile` has just registered, or re-register one
/// whose exit is being handled right now.
#[cfg(any(windows, target_os = "linux"))]
static SCAN_LOCK: Mutex<()> = Mutex::new(());

/// Milliseconds between two scans for newly started watched processes. A user
/// setting; read before every sleep, so a change applies from the next scan on.
static SCAN_INTERVAL_MS: AtomicU32 =
    AtomicU32::new(crate::settings::default_scan_interval_ms(false));

/// Whether the Windows scan reads the process list through the fast kernel
/// query instead of a Toolhelp snapshot. A user setting.
#[cfg(windows)]
static FAST_DETECTION: AtomicBool = AtomicBool::new(false);

/// Applies the watcher-related user settings to the running watcher.
pub fn apply_settings(settings: &crate::settings::AppSettings) {
    let fast_available = crate::process_watcher::fast_detection_available();
    SCAN_INTERVAL_MS.store(settings.scan_interval_ms(fast_available), Ordering::Relaxed);
    #[cfg(windows)]
    FAST_DETECTION.store(settings.fast_detection(fast_available), Ordering::Relaxed);
}

#[cfg(any(windows, target_os = "linux"))]
fn scan_interval() -> Duration {
    let ms = SCAN_INTERVAL_MS.load(Ordering::Relaxed);
    // The fast query can still fail after it was configured, which leaves the
    // scan on Toolhelp — too costly for a sub-second interval.
    #[cfg(windows)]
    let ms = if crate::process_watcher::fast_detection_available() {
        ms
    } else {
        ms.max(1000)
    };
    Duration::from_millis(ms as u64)
}

/// Start detection — no admin/root required.
///
/// Neither Windows nor Linux has a process-creation event for unprivileged
/// callers (ETW and `Win32_ProcessStartTrace` need admin, the netlink proc
/// connector needs root), so this scans the process list at the configured
/// interval. See the per-platform `scan_once` for how each keeps a pass cheap.
#[cfg(any(windows, target_os = "linux"))]
fn run_start_watcher(state: Arc<WatchState>, app: tauri::AppHandle) {
    loop {
        scan_once(&state, &app);
        std::thread::sleep(scan_interval());
    }
}

/// One pass over the process list: retires watched PIDs that are gone and
/// registers watched processes that aren't tracked yet.
///
/// Done without WMI: the kernel hands back just PID and image name per
/// process, instead of WmiPrvSE building a full `Win32_Process` object for
/// every process on each tick. Which API reads the list is the user's choice —
/// see `for_each_process_with`.
#[cfg(windows)]
fn scan_once(state: &Arc<WatchState>, app: &tauri::AppHandle) {
    use crate::process_watcher::{
        exe_path_from_pid, for_each_process_with, wide_eq_ignore_ascii_case,
    };

    let _scan = SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let watched: Vec<crate::process_watcher::WatchedProcess> = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.watched_processes.clone()
    };

    // Almost every process is irrelevant: names are compared as the raw UTF-16
    // the snapshot holds, so only a name match costs an allocation (and, below,
    // the OpenProcess round-trip an exe-path lookup takes).
    let mut current_pids: HashSet<u32> = HashSet::with_capacity(512);
    let mut candidates: Vec<(u32, String)> = Vec::new();
    let fast = FAST_DETECTION.load(Ordering::Relaxed);
    let complete = for_each_process_with(fast, |pid, name| {
        current_pids.insert(pid);
        if watched.iter().any(|w| wide_eq_ignore_ascii_case(name, w.name())) {
            candidates.push((pid, String::from_utf16_lossy(name)));
        }
    });
    // A failed snapshot says nothing about what is running; acting on it would
    // retire every watched process.
    if !complete {
        return;
    }

    // Drop dead PIDs so a restart under a reused PID is registered afresh.
    state
        .watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|pid| current_pids.contains(pid));

    // Retire watched PIDs that vanished. watch_exit normally owns this edge,
    // but it can't observe a process it never got a handle for (exited too
    // fast, or OpenProcess denied), which would otherwise pin `running`
    // non-empty forever and strand the monitor at game Hz.
    let dead: Vec<u32> = state
        .running_pids()
        .into_iter()
        .filter(|pid| !current_pids.contains(pid))
        .collect();
    for pid in dead {
        if state.on_process_stop(pid) && state.is_enabled() {
            sync_hz(state, app, "Prozess beendet".into(), None, "process_stop");
        }
    }

    let tracked: HashSet<u32> = state.running_pids().into_iter().collect();
    for (pid, name) in candidates {
        // A process can still be listed for a moment after it has terminated.
        // Registering it then would raise the Hz for something that is gone.
        if !tracked.contains(&pid) && has_exited(pid) {
            continue;
        }
        let exe = exe_path_from_pid(pid).unwrap_or_default();
        if watched.iter().any(|w| w.matches(&name, &exe)) {
            // Idempotent for a PID that is already tracked, and it picks up an
            // entry that was disabled when the process was first seen.
            register_process(state, app, &name, &exe, pid);
        }
    }
}

/// True if the process has terminated. False if that can't be told (no handle).
#[cfg(windows)]
fn has_exited(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let exited = WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
        let _ = CloseHandle(handle);
        exited
    }
}

/// Scans the running processes once, outside the regular interval. Used after
/// a config change: a process may already be running when it is added to the
/// watch list, and the Hz should follow right away rather than a tick later.
#[cfg(any(windows, target_os = "linux"))]
pub fn reconcile(state: &Arc<WatchState>, app: &tauri::AppHandle) {
    scan_once(state, app);
}

/// One running process, as read from `/proc`.
#[cfg(target_os = "linux")]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
}

/// Every process with its name and executable path, for the process picker.
/// The watcher itself doesn't use this: resolving every executable path is the
/// expensive part, and `scan_once` only does it for watch-list candidates.
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
        let exe_path = exe_path_linux(pid);
        let name = linux_process_name(pid, &exe_path);
        if name.is_empty() {
            continue;
        }
        result.push(ProcInfo { pid, name, exe_path });
    }
    result
}

/// The kernel truncates `/proc/<pid>/comm` to this many bytes.
#[cfg(target_os = "linux")]
const COMM_MAX: usize = 15;

/// Target of the `/proc/<pid>/exe` symlink; empty if it can't be read (kernel
/// threads, other users' processes, or the process is already gone).
#[cfg(target_os = "linux")]
fn exe_path_linux(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads `/proc/<pid>/comm` into `buf`, reusing `path` for the file name so a
/// pass over every process allocates nothing. `None` if the process is gone or
/// its name isn't valid UTF-8.
#[cfg(target_os = "linux")]
fn read_comm<'a>(pid: u32, path: &mut String, buf: &'a mut [u8; 64]) -> Option<&'a str> {
    use std::fmt::Write as _;
    use std::io::Read as _;

    path.clear();
    let _ = write!(path, "/proc/{pid}/comm");
    let len = std::fs::File::open(path.as_str()).ok()?.read(buf).ok()?;
    std::str::from_utf8(&buf[..len]).ok().map(str::trim)
}

/// The name a process is matched by.
///
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
fn untruncated_name(comm: &str, exe_path: &str) -> String {
    if comm.len() == COMM_MAX {
        if let Some(base) = std::path::Path::new(exe_path)
            .file_name()
            .map(|b| b.to_string_lossy().into_owned())
        {
            if base.len() > comm.len() && base.starts_with(comm) {
                return base;
            }
        }
    }
    comm.to_string()
}

/// `untruncated_name` of a running process; empty if it has no readable name.
#[cfg(target_os = "linux")]
fn linux_process_name(pid: u32, exe_path: &str) -> String {
    let (mut path, mut buf) = (String::new(), [0u8; 64]);
    read_comm(pid, &mut path, &mut buf)
        .map(|comm| untruncated_name(comm, exe_path))
        .unwrap_or_default()
}

/// Whether a process whose `comm` is `comm` could be the one `watched` names:
/// the names are equal, or `comm` is the truncated start of a longer watched
/// name. Never false for a process `untruncated_name` would make match, so it
/// can rule a process out before its executable path is resolved.
#[cfg(target_os = "linux")]
fn comm_may_match(comm: &str, watched: &str) -> bool {
    comm.eq_ignore_ascii_case(watched)
        || (comm.len() == COMM_MAX
            && watched.len() > COMM_MAX
            && watched.as_bytes()[..COMM_MAX].eq_ignore_ascii_case(comm.as_bytes()))
}

/// True if the process has exited but its parent hasn't reaped it yet. Such a
/// zombie keeps its `/proc` entry and its name — for as long as a sloppy
/// launcher leaves it there — but it is no longer running.
#[cfg(target_os = "linux")]
fn is_zombie(pid: u32) -> bool {
    // `pid (comm) state ...`; comm may itself contain parentheses.
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let (_, rest) = stat.rsplit_once(')')?;
            Some(rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Reads `/proc` once: the PIDs of all processes, and the processes matching
/// the watch list that aren't in `skip`. `None` if `/proc` can't be read.
///
/// Every process's `comm` is read on every pass, even for PIDs seen before:
/// a process changes its name when it `exec`s (a launcher script turning into
/// the game keeps its PID), so remembering a PID as "not watched" would miss
/// it. What is saved is the `exe` symlink lookup, which only candidates get.
#[cfg(target_os = "linux")]
fn scan_proc(
    watched: &[crate::process_watcher::WatchedProcess],
    skip: &HashSet<u32>,
) -> Option<(HashSet<u32>, Vec<ProcInfo>)> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut pids: HashSet<u32> = HashSet::with_capacity(512);
    let mut matches = Vec::new();
    let (mut path, mut buf) = (String::with_capacity(32), [0u8; 64]);

    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        pids.insert(pid);
        if skip.contains(&pid) {
            continue;
        }
        let Some(comm) = read_comm(pid, &mut path, &mut buf) else {
            continue;
        };
        if !watched.iter().any(|w| comm_may_match(comm, w.name())) || is_zombie(pid) {
            continue;
        }
        let exe_path = exe_path_linux(pid);
        let name = untruncated_name(comm, &exe_path);
        if watched.iter().any(|w| w.matches(&name, &exe_path)) {
            matches.push(ProcInfo { pid, name, exe_path });
        }
    }
    Some((pids, matches))
}

/// One pass over `/proc`: retires watched PIDs that are gone and registers
/// watched processes that aren't tracked yet.
#[cfg(target_os = "linux")]
fn scan_once(state: &Arc<WatchState>, app: &tauri::AppHandle) {
    let _scan = SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let watched: Vec<crate::process_watcher::WatchedProcess> = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.watched_processes.clone()
    };
    // Processes already counted as running need no second look.
    let tracked: HashSet<u32> = state.running_pids().into_iter().collect();
    // An unreadable /proc says nothing about what is running; acting on it
    // would retire every watched process.
    let Some((current_pids, matches)) = scan_proc(&watched, &tracked) else {
        return;
    };

    // Drop dead PIDs so a restart under a reused PID is registered afresh.
    state
        .watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|pid| current_pids.contains(pid));

    // Retire watched PIDs that vanished. watch_exit normally owns this edge,
    // but it can't observe a process it couldn't get a pidfd for (kernel older
    // than 5.3, or the process exited first), which would otherwise pin
    // `running` non-empty forever and strand the monitor at game Hz.
    for pid in tracked {
        let gone = !current_pids.contains(&pid) || is_zombie(pid);
        if gone && state.on_process_stop(pid) && state.is_enabled() {
            sync_hz(state, app, "Prozess beendet".into(), None, "process_stop");
        }
    }

    for proc in &matches {
        register_process(state, app, &proc.name, &proc.exe_path, proc.pid);
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
#[allow(clippy::too_many_arguments)]
fn set_monitor_hz(
    state: &WatchState,
    app: &tauri::AppHandle,
    monitor: &str,
    target: u32,
    reason: String,
    process_name: Option<String>,
    event_type: &str,
    batch: u64,
) {
    let _guard = state.hz_lock.lock().unwrap_or_else(|e| e.into_inner());
    let prev = crate::display::get_current_refresh_rate(monitor);
    crate::logging::log(&format!(
        "set_monitor_hz: monitor='{monitor}' prev={prev}Hz target={target}Hz reason='{reason}' event={event_type}"
    ));
    // A configured monitor that is unplugged reports no mode at all; switching
    // it would only raise an error for a display the user can't see.
    if prev == 0 {
        crate::logging::log("  -> skip: monitor not connected");
        return;
    }
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
        "monitor": monitor,
        "batch": batch,
    });
    if let Some(name) = process_name {
        payload["process_name"] = serde_json::json!(name);
    }
    emit_hz_changed(app, payload);
}

/// Computes the target Hz of every managed monitor from the *current* running
/// set and config, then applies them. Whoever runs last during a race wins
/// with the right values.
pub fn sync_hz(
    state: &WatchState,
    app: &tauri::AppHandle,
    reason: String,
    process_name: Option<String>,
    event_type: &str,
) {
    let running = state.get_running();
    let targets = state
        .config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .hz_targets(&running);
    crate::logging::log(&format!(
        "sync_hz: running={running:?} -> targets={targets:?} (event={event_type}, reason='{reason}')"
    ));
    apply_targets(state, app, &targets, reason, process_name, event_type);
}

/// Source of `batch` ids: every monitor switched by one sync shares one, so the
/// UI can count a game start as one switch however many monitors it touched.
static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);

/// Applies each `(monitor, hz)` pair as one batch.
pub fn apply_targets(
    state: &WatchState,
    app: &tauri::AppHandle,
    targets: &[(String, u32)],
    reason: String,
    process_name: Option<String>,
    event_type: &str,
) {
    let batch = NEXT_BATCH.fetch_add(1, Ordering::Relaxed);
    for (monitor, hz) in targets {
        set_monitor_hz(
            state,
            app,
            monitor,
            *hz,
            reason.clone(),
            process_name.clone(),
            event_type,
            batch,
        );
    }
}

/// Shared tail once a watched process's exit has been detected: release the
/// PID (so a restart re-arms a fresh `watch_exit` thread) and fire the
/// empty-set edge if this was the last running instance.
#[cfg(any(windows, target_os = "linux"))]
fn on_watched_process_exited(pid: u32, name: String, state: Arc<WatchState>, app: tauri::AppHandle) {
    // Not while a scan is mid-pass: its snapshot may still list this PID, and
    // it would register the process again right after we retire it.
    let _scan = SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        // Either the process is already gone or it refuses the handle. Those
        // can't be told apart here, and reporting an exit for a process that
        // is still running would drop the Hz under a running game — so leave
        // the PID tracked; the next scan retires it once it has really gone.
        crate::logging::log(&format!(
            "watch_exit: no handle for '{name}' pid={pid}; the scan will detect its exit"
        ));
        return;
    };
    unsafe {
        WaitForSingleObject(handle, u32::MAX); // INFINITE
        let _ = CloseHandle(handle);
    }

    on_watched_process_exited(pid, name, state, app);
}

/// Blocks until the process exits, on a pidfd — a kernel event, zero CPU
/// overhead, and no root needed.
#[cfg(target_os = "linux")]
fn watch_exit(pid: u32, name: String, state: Arc<WatchState>, app: tauri::AppHandle) {
    if !wait_for_exit(pid) {
        // No pidfd (kernel older than 5.3) or the process is already gone.
        // Those can't be told apart here, and reporting an exit for a process
        // that is still running would drop the Hz under a running game — so
        // leave the PID tracked; the next scan retires it once it has gone.
        crate::logging::log(&format!(
            "watch_exit: no pidfd for '{name}' pid={pid}; the scan will detect its exit"
        ));
        return;
    }

    on_watched_process_exited(pid, name, state, app);
}

/// Waits for `pid` to exit. False if it can't be waited on at all.
#[cfg(target_os = "linux")]
fn wait_for_exit(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // pidfd_open has no glibc wrapper before 2.36; the raw syscall works on
    // every kernel that has it (5.3+) and fails cleanly with ENOSYS otherwise.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0 as libc::c_uint) } as libc::c_int;
    if fd < 0 {
        return false;
    }

    // A pidfd turns readable when its process terminates.
    let mut poll_fd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let exited = loop {
        let ready = unsafe { libc::poll(&mut poll_fd, 1, -1) };
        if ready > 0 {
            break true;
        }
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break false;
    };
    unsafe { libc::close(fd) };
    exited
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn own_exe() -> String {
        std::env::current_exe().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn own_process_name_is_the_full_binary_name() {
        // The test binary is `app_lib-<hash>`, longer than the 15-byte `comm`
        // limit, so this exercises the untruncation path.
        let exe = own_exe();
        let base = std::path::Path::new(&exe).file_name().unwrap().to_string_lossy().into_owned();
        assert!(base.len() > 15, "expected a long test binary name, got {base}");
        assert_eq!(linux_process_name(std::process::id(), &exe), base);
    }

    #[test]
    fn truncated_comm_is_kept_when_exe_does_not_extend_it() {
        // Script/Wine case: the exe is an interpreter, so `comm` stays.
        let pid = std::process::id();
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap();
        assert_eq!(linux_process_name(pid, "/usr/bin/bash"), comm.trim());
        assert_eq!(linux_process_name(pid, ""), comm.trim());
    }

    #[test]
    fn unknown_pids_have_no_name() {
        assert_eq!(linux_process_name(u32::MAX, "/usr/bin/whatever"), "");
    }

    fn named(name: &str) -> crate::process_watcher::WatchedProcess {
        crate::process_watcher::WatchedProcess::Name(name.into())
    }

    #[test]
    fn comm_prefilter_never_rules_out_a_real_match() {
        assert!(comm_may_match("game", "Game"));
        assert!(!comm_may_match("game", "game2"));
        // A 15-byte comm may be the truncated start of a longer watched name.
        assert!(comm_may_match("a-very-long-nam", "A-Very-Long-Name-Indeed"));
        assert!(!comm_may_match("a-very-long-nam", "a-very-long-nom-indeed"));
        // A shorter comm is never a truncation.
        assert!(!comm_may_match("a-very-long", "a-very-long-name-indeed"));
        // Non-ASCII names must not trip up the byte-wise prefix check.
        assert!(!comm_may_match("a-very-long-nam", "üüüüüüüüüüüüüüüü"));
    }

    #[test]
    fn untruncated_name_only_extends_a_full_length_comm() {
        assert_eq!(untruncated_name("a-very-long-nam", "/opt/a-very-long-name"), "a-very-long-name");
        assert_eq!(untruncated_name("a-very-long-nam", "/usr/bin/bash"), "a-very-long-nam");
        assert_eq!(untruncated_name("short", "/opt/shorter"), "short");
    }

    #[test]
    fn scan_finds_this_process_by_its_full_name() {
        let pid = std::process::id();
        let exe = own_exe();
        let base = std::path::Path::new(&exe).file_name().unwrap().to_string_lossy().into_owned();

        let (pids, matches) = scan_proc(&[named(&base)], &HashSet::new()).unwrap();
        assert!(pids.contains(&pid));
        let me = matches.iter().find(|p| p.pid == pid).expect("own process not matched");
        assert_eq!(me.name, base);
        assert_eq!(me.exe_path, exe);

        // A tracked PID is listed but not matched again.
        let (pids, matches) = scan_proc(&[named(&base)], &HashSet::from([pid])).unwrap();
        assert!(pids.contains(&pid));
        assert!(matches.iter().all(|p| p.pid != pid));

        // Nothing matches a name no process has.
        let (_, matches) = scan_proc(&[named("no-such-process-xyz")], &HashSet::new()).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn scan_follows_a_process_that_changes_its_name_by_exec() {
        // A shell that sleeps and then `exec`s keeps its PID but changes name:
        // the scan must match it under the new name, not the remembered one.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 0.3; exec sleep 5"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let matched = |name: &str| {
            let (_, m) = scan_proc(&[named(name)], &HashSet::new()).unwrap();
            m.iter().any(|p| p.pid == pid)
        };

        let mut seen_as_sh = false;
        let mut seen_as_sleep = false;
        for _ in 0..100 {
            seen_as_sh |= matched("sh");
            if matched("sleep") {
                seen_as_sleep = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen_as_sh, "never saw the shell before its exec");
        assert!(seen_as_sleep, "the exec'd process was not matched under its new name");
    }

    #[test]
    fn zombies_are_not_matched() {
        // Exited but not yet waited for: still in /proc under its name.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut zombie = false;
        for _ in 0..100 {
            zombie = is_zombie(pid);
            if zombie {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (pids, matches) = scan_proc(&[named("true")], &HashSet::new()).unwrap();
        let _ = child.wait();

        assert!(zombie, "child never became a zombie");
        assert!(pids.contains(&pid), "a zombie still has a /proc entry");
        assert!(matches.iter().all(|p| p.pid != pid), "a zombie was matched as running");
        assert!(!is_zombie(std::process::id()));
        assert!(!is_zombie(u32::MAX));
    }

    #[test]
    fn wait_for_exit_returns_when_the_process_ends() {
        let mut child = std::process::Command::new("sleep").arg("0.3").spawn().unwrap();
        let started = std::time::Instant::now();
        assert!(wait_for_exit(child.id()), "pidfd wait unavailable on this kernel");
        let waited = started.elapsed();
        let _ = child.wait();
        assert!(waited >= Duration::from_millis(200), "returned early after {waited:?}");
        assert!(waited < Duration::from_secs(3), "returned late after {waited:?}");

        // A PID that doesn't exist can't be waited on.
        assert!(!wait_for_exit(u32::MAX));
        assert!(!wait_for_exit(i32::MAX as u32));
    }

    #[test]
    fn process_list_contains_this_process() {
        let pid = std::process::id();
        let me = list_processes_linux().into_iter().find(|p| p.pid == pid).unwrap();
        assert_eq!(me.exe_path, own_exe());
        assert!(!me.name.is_empty());
    }
}
