use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Resolves an exe path from a PID via QueryFullProcessImageNameW.
/// Works for elevated and most protected processes; `None` for those that refuse even limited query access.
#[cfg(windows)]
pub(crate) fn exe_path_from_pid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::core::PWSTR;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        ok.ok().map(|_| String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Calls `f` with the PID and image name (UTF-16, no terminator) of every
/// running process, straight from a Toolhelp snapshot. Needs no privileges and
/// sees elevated and protected processes too. Returns false if the snapshot
/// couldn't be taken or read, in which case `f` may have seen nothing.
#[cfg(windows)]
pub(crate) fn for_each_process(mut f: impl FnMut(u32, &[u16])) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        let any = more;
        while more {
            let name = &entry.szExeFile;
            let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            f(entry.th32ProcessID, &name[..len]);
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        any
    }
}

/// Whether this system supports the fast process query (Windows 11 24H2 and
/// later). Tested once, on first use; never true on other platforms.
pub(crate) fn fast_detection_available() -> bool {
    #[cfg(windows)]
    {
        fast_list::available()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// `for_each_process` through the API the user picked: the fast kernel query
/// if `fast` is set and this Windows supports it, the Toolhelp snapshot
/// otherwise.
#[cfg(windows)]
pub(crate) fn for_each_process_with(fast: bool, mut f: impl FnMut(u32, &[u16])) -> bool {
    if fast {
        if let Some(complete) = fast_list::for_each_process(&mut f) {
            return complete;
        }
    }
    for_each_process(f)
}

/// The "performance" process listing: `NtQuerySystemInformation` with the
/// `SystemBasicProcessInformation` class, which returns nothing but PID,
/// parent and image name per process. A Toolhelp snapshot is built on the full
/// `SystemProcessInformation` class instead, which also collects every thread
/// of every process — some hundred times the work for the same two fields.
///
/// The class only exists from Windows 11 24H2 on and is not part of the
/// documented API, so nothing here is taken on trust: the first listing must
/// contain this very process under its own name, or the method is switched off
/// for the rest of the run and the caller falls back to Toolhelp.
#[cfg(windows)]
mod fast_list {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Mutex;

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQuerySystemInformation(
            class: u32,
            info: *mut core::ffi::c_void,
            len: u32,
            return_len: *mut u32,
        ) -> i32;
    }

    const SYSTEM_BASIC_PROCESS_INFORMATION: u32 = 252;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

    /// `SYSTEM_BASICPROCESS_INFORMATION`. The name is a `UNICODE_STRING` whose
    /// buffer points into the same result buffer.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Entry {
        next_entry_offset: u32,
        process_id: usize,
        parent_process_id: usize,
        sequence_number: u64,
        name_bytes: u16,
        name_capacity: u16,
        name: *const u16,
    }

    const UNTESTED: u8 = 0;
    const WORKS: u8 = 1;
    const UNAVAILABLE: u8 = 2;
    static STATE: AtomicU8 = AtomicU8::new(UNTESTED);

    /// Result buffer, kept between scans (a few dozen KB). `u64` elements so
    /// the entries the kernel writes are suitably aligned.
    static BUFFER: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    /// Calls `f` for every process. `None` if this method is unavailable, in
    /// which case `f` was not called; `Some(false)` if a listing went wrong
    /// part-way.
    pub(crate) fn for_each_process(f: &mut impl FnMut(u32, &[u16])) -> Option<bool> {
        if !available() {
            return None;
        }
        let complete = list(f);
        if !complete {
            // It passed its check once and broke later: don't keep trying.
            STATE.store(UNAVAILABLE, Ordering::Relaxed);
            crate::logging::log("process detection: fast kernel query failed, using Toolhelp");
        }
        Some(complete)
    }

    /// Whether the query works here. Tested on the first call; a later
    /// failure also turns it off.
    pub(crate) fn available() -> bool {
        match STATE.load(Ordering::Relaxed) {
            WORKS => true,
            UNAVAILABLE => false,
            _ => {
                let works = lists_this_process();
                STATE.store(if works { WORKS } else { UNAVAILABLE }, Ordering::Relaxed);
                crate::logging::log(if works {
                    "process detection: fast kernel query available"
                } else {
                    "process detection: fast kernel query unavailable"
                });
                works
            }
        }
    }

    /// True if a listing completes and names this process correctly.
    fn lists_this_process() -> bool {
        let Some(own_name) = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        else {
            return false;
        };
        let own_pid = std::process::id();
        let mut found = false;
        let complete = list(&mut |pid, name| {
            found |= pid == own_pid && super::wide_eq_ignore_ascii_case(name, &own_name);
        });
        complete && found
    }

    fn list(f: &mut impl FnMut(u32, &[u16])) -> bool {
        let mut buf = BUFFER.lock().unwrap_or_else(|e| e.into_inner());
        if buf.is_empty() {
            buf.resize(8 * 1024, 0); // 64 KB
        }

        // The process list can grow between the size probe and the next call.
        let mut written = 0u32;
        let mut ok = false;
        for _ in 0..4 {
            let capacity = (buf.len() * 8) as u32;
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_BASIC_PROCESS_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    capacity,
                    &mut written,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                let wanted = (written.max(capacity) as usize + 16 * 1024).min(16 << 20);
                buf.resize(wanted / 8, 0);
                continue;
            }
            ok = status >= 0 && written <= capacity;
            break;
        }
        if !ok {
            return false;
        }

        // Every offset and pointer below comes from the kernel's answer; each
        // is checked against the bytes actually written before it is used.
        let base = buf.as_ptr() as usize;
        let end = base + written as usize;
        let mut at = base;
        loop {
            if !at.is_multiple_of(8) || at + std::mem::size_of::<Entry>() > end {
                return false;
            }
            let entry = unsafe { *(at as *const Entry) };

            let name_start = entry.name as usize;
            let name_bytes = entry.name_bytes as usize;
            let name: &[u16] = if name_bytes == 0 {
                &[]
            } else if name_start >= base && name_start.is_multiple_of(2) && name_start + name_bytes <= end {
                unsafe { std::slice::from_raw_parts(entry.name, name_bytes / 2) }
            } else {
                return false;
            };
            f(entry.process_id as u32, name);

            if entry.next_entry_offset == 0 {
                return true;
            }
            at += entry.next_entry_offset as usize;
        }
    }
}

/// `str::eq_ignore_ascii_case` for a UTF-16 name against a `&str`, so a process
/// name can be checked against the watch list without decoding it first.
#[cfg(any(windows, test))]
pub(crate) fn wide_eq_ignore_ascii_case(wide: &[u16], s: &str) -> bool {
    fn lower(c: u16) -> u16 {
        if (b'A' as u16..=b'Z' as u16).contains(&c) {
            c + 32
        } else {
            c
        }
    }
    wide.iter().map(|&c| lower(c)).eq(s.encode_utf16().map(lower))
}

/// Legacy per-monitor Hz pair, read only to migrate old configs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorHz {
    pub game_hz: u32,
    pub default_hz: u32,
}

/// Global settings for one monitor, keyed by device name in `WatchConfig::monitors`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MonitorProfile {
    /// Whether a watched game switches this monitor unless the game says otherwise.
    pub enabled: bool,
    pub game_hz: u32,
    pub default_hz: u32,
}

/// A game's deviation from a monitor's global setting. Serialized as `"keep"`
/// or `{"hz": 90}`; an absent entry means "use the global setting".
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HzOverride {
    /// Leave this monitor alone while the game runs.
    Keep,
    Hz(u32),
}

/// A watched process entry. Stored as a plain string for name-only entries so
/// existing config files (Vec<String>) deserialize without migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WatchedProcess {
    Name(String),
    WithPath { name: String, path: String },
}

impl WatchedProcess {
    /// The process name this entry watches. A name match is necessary for the
    /// entry to match at all, so failing it rules a process out without
    /// resolving its executable path.
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) | Self::WithPath { name: n, .. } => n,
        }
    }

    /// Unique key used as HashMap key in running/counts maps.
    pub fn key(&self) -> String {
        match self {
            Self::Name(n) => n.to_lowercase(),
            Self::WithPath { name: n, path: p } => {
                format!("{}|{}", n.to_lowercase(), p.to_lowercase())
            }
        }
    }

    /// True if this entry matches the given process name and executable path.
    pub fn matches(&self, proc_name: &str, exe_path: &str) -> bool {
        match self {
            Self::Name(n) => n.eq_ignore_ascii_case(proc_name),
            Self::WithPath { name: n, path: p } => {
                n.eq_ignore_ascii_case(proc_name)
                    && exe_path.to_lowercase().contains(&p.to_lowercase())
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct WatchConfig {
    pub watched_processes: Vec<WatchedProcess>,
    /// Device name → global profile. Sorted, so "first enabled monitor" is
    /// stable across the backend and the UI.
    #[serde(default)]
    pub monitors: BTreeMap<String, MonitorProfile>,
    /// Keys (see `WatchedProcess::key`) of entries that stay in the list but
    /// are paused: they never count as running and so never raise the Hz.
    #[serde(default)]
    pub disabled_processes: Vec<String>,
    /// Entry key → device name → override.
    #[serde(default)]
    pub process_overrides: HashMap<String, HashMap<String, HzOverride>>,

    // Single-monitor layout from before `monitors`. Read once by `parse`, never written.
    #[serde(default, skip_serializing)]
    monitor_name: String,
    #[serde(default, skip_serializing)]
    game_hz: Option<u32>,
    #[serde(default, skip_serializing)]
    default_hz: Option<u32>,
    #[serde(default, skip_serializing)]
    monitor_settings: HashMap<String, MonitorHz>,
}

impl WatchConfig {
    /// Parses a stored config, upgrading the single-monitor layout: the
    /// monitor that was selected stays the only one that switches.
    pub fn parse(json: &str) -> serde_json::Result<Self> {
        let mut cfg: Self = serde_json::from_str(json)?;
        let selected = std::mem::take(&mut cfg.monitor_name);
        let legacy = std::mem::take(&mut cfg.monitor_settings);
        let (game_hz, default_hz) = (cfg.game_hz.take(), cfg.default_hz.take());
        if cfg.monitors.is_empty() {
            for (name, hz) in legacy {
                cfg.monitors.insert(
                    name.clone(),
                    MonitorProfile {
                        enabled: name == selected,
                        game_hz: hz.game_hz,
                        default_hz: hz.default_hz,
                    },
                );
            }
            if !selected.is_empty() && !cfg.monitors.contains_key(&selected) {
                cfg.monitors.insert(
                    selected,
                    MonitorProfile {
                        enabled: true,
                        game_hz: game_hz.unwrap_or(144),
                        default_hz: default_hz.unwrap_or(60),
                    },
                );
            }
        }
        Ok(cfg)
    }

    pub fn is_disabled(&self, key: &str) -> bool {
        self.disabled_processes.iter().any(|d| d == key)
    }

    /// The monitor the status view and tray report on: the first one that
    /// switches, else the first one configured at all.
    pub fn status_monitor(&self) -> Option<&str> {
        self.monitors
            .iter()
            .find(|(_, p)| p.enabled)
            .or_else(|| self.monitors.iter().next())
            .map(|(name, _)| name.as_str())
    }

    /// Target Hz per monitor while the entries in `running` (keys) run.
    ///
    /// A monitor is managed if it switches globally or any game sets an
    /// explicit Hz for it; unmanaged monitors are left out entirely. Each
    /// running game wants its override, else the global game Hz if the
    /// monitor switches globally, else nothing. The highest wish wins, so a
    /// low-Hz game never drags down one running alongside it; with no wish
    /// the monitor goes back to its default.
    pub fn hz_targets(&self, running: &[String]) -> Vec<(String, u32)> {
        self.monitors
            .iter()
            .filter(|(mon, p)| {
                p.enabled
                    || self
                        .process_overrides
                        .values()
                        .any(|o| matches!(o.get(*mon), Some(HzOverride::Hz(_))))
            })
            .map(|(mon, p)| {
                let wanted = running.iter().filter_map(|key| {
                    match self.process_overrides.get(key).and_then(|o| o.get(mon)) {
                        Some(HzOverride::Keep) => None,
                        Some(HzOverride::Hz(hz)) => Some(*hz),
                        None => p.enabled.then_some(p.game_hz),
                    }
                });
                (mon.clone(), wanted.max().unwrap_or(p.default_hz))
            })
            .collect()
    }
}

pub struct WatchState {
    /// PID → WatchedProcess key of every currently-running watched instance.
    running: Arc<Mutex<HashMap<u32, String>>>,
    pub config: Arc<Mutex<WatchConfig>>,
    process_counts: Arc<Mutex<HashMap<String, u32>>>,
    pub enabled: Arc<AtomicBool>,
    pub hz_lock: Arc<Mutex<()>>,
    pub watching: Arc<Mutex<HashSet<u32>>>,
}

impl WatchState {
    /// `enabled` is passed in rather than defaulted so the watcher never runs a
    /// single edge with the wrong value between construction and restore.
    pub fn new(config: WatchConfig, enabled: bool) -> Self {
        let counts = config
            .watched_processes
            .iter()
            .map(|p| (p.key(), 0u32))
            .collect();
        Self {
            running: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(config)),
            process_counts: Arc::new(Mutex::new(counts)),
            enabled: Arc::new(AtomicBool::new(enabled)),
            hz_lock: Arc::new(Mutex::new(())),
            watching: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Returns true when the first instance of a watched entry starts — the
    /// set of running entries changed, so the Hz targets must be recomputed.
    /// Idempotent per PID.
    pub fn on_process_start(&self, name: &str, exe_path: &str, pid: u32) -> bool {
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let matched = config
            .watched_processes
            .iter()
            .find(|p| p.matches(name, exe_path) && !config.is_disabled(&p.key()));
        let Some(entry) = matched else { return false };
        let key = entry.key();
        drop(config);

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains_key(&pid) {
            return false;
        }
        let first_of_entry = !running.values().any(|k| *k == key);
        running.insert(pid, key.clone());
        drop(running);

        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(key).or_insert(0) += 1;

        first_of_entry
    }

    /// Returns true when the last instance of a watched entry stops.
    pub fn on_process_stop(&self, pid: u32) -> bool {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = running.remove(&pid) {
            let last_of_entry = !running.values().any(|k| *k == key);
            drop(running);
            let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = counts.get_mut(&key) {
                *c = c.saturating_sub(1);
            }
            last_of_entry
        } else {
            false
        }
    }

    /// PIDs of all currently-tracked watched instances.
    pub fn running_pids(&self) -> Vec<u32> {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    pub fn is_any_running(&self) -> bool {
        !self.running.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// Returns unique WatchedProcess keys of currently running entries.
    pub fn get_running(&self) -> Vec<String> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let unique: HashSet<String> = running.values().cloned().collect();
        unique.into_iter().collect()
    }

    pub fn get_process_counts(&self) -> HashMap<String, u32> {
        self.process_counts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Atomically replaces config and prunes running entries no longer in the
    /// new config or now disabled. Callers re-derive the target Hz via
    /// `sync_hz` afterwards, so nothing needs to be returned here.
    pub fn update_config(&self, config: WatchConfig) {
        let new_watched: HashSet<String> =
            config.watched_processes.iter().map(|p| p.key()).collect();
        let active: HashSet<String> = new_watched
            .iter()
            .filter(|k| !config.is_disabled(k))
            .cloned()
            .collect();
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|_pid, key| active.contains(key));
        drop(running);

        // Counts are keyed by watched entry, so anything no longer watched is
        // dead weight — dropping it keeps the map from growing without bound.
        // Surviving keys are recounted from `running` so a removed-then-readded
        // entry can't resurrect a stale count.
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let mut fresh: HashMap<String, u32> =
            new_watched.iter().map(|k| (k.clone(), 0u32)).collect();
        for key in running.values() {
            if let Some(c) = fresh.get_mut(key) {
                *c += 1;
            }
        }
        drop(running);
        *self.process_counts.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(names: &[&str]) -> WatchConfig {
        WatchConfig {
            watched_processes: names
                .iter()
                .map(|n| WatchedProcess::Name((*n).into()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn wide_names_compare_like_eq_ignore_ascii_case() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert!(wide_eq_ignore_ascii_case(&wide("Game.EXE"), "game.exe"));
        assert!(wide_eq_ignore_ascii_case(&wide("spiel-ü.exe"), "Spiel-ü.exe"));
        assert!(wide_eq_ignore_ascii_case(&wide(""), ""));
        // Only ASCII folds, exactly like `str::eq_ignore_ascii_case`.
        assert!(!wide_eq_ignore_ascii_case(&wide("spiel-Ü.exe"), "spiel-ü.exe"));
        // A prefix in either direction is not a match.
        assert!(!wide_eq_ignore_ascii_case(&wide("game.exe"), "game.exe2"));
        assert!(!wide_eq_ignore_ascii_case(&wide("game.exe2"), "game.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn process_snapshot_contains_this_process() {
        let me = std::process::id();
        let exe = std::env::current_exe().unwrap();
        let base = exe.file_name().unwrap().to_string_lossy().into_owned();
        let mut found = None;
        assert!(for_each_process(|pid, name| {
            if pid == me {
                found = Some(String::from_utf16_lossy(name));
            }
        }));
        assert_eq!(found.as_deref(), Some(base.as_str()));
    }

    /// The fast listing and the Toolhelp snapshot must describe the same
    /// processes. On a Windows without the fast query there is nothing to
    /// compare, and `for_each_process_with` is covered by the test above.
    #[cfg(windows)]
    #[test]
    fn fast_listing_agrees_with_the_snapshot() {
        let mut fast: HashMap<u32, String> = HashMap::new();
        let Some(complete) = fast_list::for_each_process(&mut |pid, name| {
            fast.insert(pid, String::from_utf16_lossy(name));
        }) else {
            return;
        };
        assert!(complete);

        let mut snapshot: HashMap<u32, String> = HashMap::new();
        assert!(for_each_process(|pid, name| {
            snapshot.insert(pid, String::from_utf16_lossy(name));
        }));

        // Processes come and go between the two listings, so compare the PIDs
        // both saw: those must carry the same name (PID 0 is the idle
        // pseudo-process, which the two APIs label differently).
        let shared: Vec<u32> = fast
            .keys()
            .filter(|pid| **pid != 0 && snapshot.contains_key(pid))
            .copied()
            .collect();
        assert!(shared.len() * 10 >= snapshot.len() * 9, "listings barely overlap");
        for pid in shared {
            assert_eq!(fast[&pid], snapshot[&pid], "pid {pid}");
        }
    }

    #[test]
    fn start_and_stop_edges_fire_once() {
        let s = WatchState::new(cfg(&["game.exe"]), true);

        // First instance is the empty -> non-empty edge; a second is not.
        assert!(s.on_process_start("game.exe", "", 1));
        assert!(!s.on_process_start("game.exe", "", 2));
        // Same PID twice must not double-count.
        assert!(!s.on_process_start("game.exe", "", 2));
        assert_eq!(s.get_process_counts()["game.exe"], 2);

        // Only the last instance leaving is the down edge.
        assert!(!s.on_process_stop(1));
        assert!(s.on_process_stop(2));
        // An unknown PID is a no-op, not an edge.
        assert!(!s.on_process_stop(999));
        assert!(!s.is_any_running());
        assert_eq!(s.get_process_counts()["game.exe"], 0);
    }

    #[test]
    fn each_entry_fires_its_own_edges() {
        let s = WatchState::new(cfg(&["a.exe", "b.exe"]), true);
        assert!(s.on_process_start("a.exe", "", 1));
        // A second game changes the running set, so it must re-sync too.
        assert!(s.on_process_start("b.exe", "", 2));
        assert!(s.on_process_stop(2));
        assert!(s.on_process_stop(1));
    }

    fn profile(enabled: bool, game_hz: u32, default_hz: u32) -> MonitorProfile {
        MonitorProfile { enabled, game_hz, default_hz }
    }

    #[test]
    fn hz_targets_follow_globals_and_overrides() {
        let mut c = cfg(&["a.exe", "b.exe"]);
        c.monitors.insert("M1".into(), profile(true, 144, 60));
        c.monitors.insert("M2".into(), profile(false, 165, 60));
        c.monitors.insert("M3".into(), profile(true, 120, 60));
        c.process_overrides.insert(
            "a.exe".into(),
            HashMap::from([("M1".into(), HzOverride::Hz(90)), ("M3".into(), HzOverride::Keep)]),
        );
        c.process_overrides
            .insert("b.exe".into(), HashMap::from([("M2".into(), HzOverride::Hz(165))]));

        // Idle: every managed monitor sits at its default; M2 is managed only
        // because b.exe overrides it.
        let idle = c.hz_targets(&[]);
        assert_eq!(idle, vec![("M1".into(), 60), ("M2".into(), 60), ("M3".into(), 60)]);

        // a.exe alone: its own Hz on M1, M2 isn't switched globally, M3 kept at default.
        let a = c.hz_targets(&["a.exe".into()]);
        assert_eq!(a, vec![("M1".into(), 90), ("M2".into(), 60), ("M3".into(), 60)]);

        // Both: the higher wish wins per monitor.
        let both = c.hz_targets(&["a.exe".into(), "b.exe".into()]);
        assert_eq!(both, vec![("M1".into(), 144), ("M2".into(), 165), ("M3".into(), 120)]);
    }

    #[test]
    fn unmanaged_monitors_are_left_alone() {
        let mut c = cfg(&["a.exe"]);
        c.monitors.insert("M1".into(), profile(false, 144, 60));
        assert!(c.hz_targets(&["a.exe".into()]).is_empty());
        assert_eq!(c.status_monitor(), Some("M1"));
    }

    #[test]
    fn legacy_config_migrates_to_the_selected_monitor() {
        let json = r#"{
            "watched_processes": ["a.exe"],
            "monitor_name": "M2",
            "game_hz": 144,
            "default_hz": 60,
            "monitor_settings": {
                "M1": { "game_hz": 75, "default_hz": 60 },
                "M2": { "game_hz": 165, "default_hz": 120 }
            }
        }"#;
        let c = WatchConfig::parse(json).unwrap();
        assert_eq!(c.monitors["M1"], profile(false, 75, 60));
        assert_eq!(c.monitors["M2"], profile(true, 165, 120));
        assert_eq!(c.status_monitor(), Some("M2"));

        // Selected monitor without saved settings falls back to the old globals.
        let c = WatchConfig::parse(
            r#"{"watched_processes":[],"monitor_name":"M1","game_hz":144,"default_hz":60}"#,
        )
        .unwrap();
        assert_eq!(c.monitors["M1"], profile(true, 144, 60));

        // The legacy fields are never written back.
        let out = serde_json::to_string(&c).unwrap();
        assert!(!out.contains("monitor_name") && !out.contains("monitor_settings"));
    }

    #[test]
    fn override_serialization() {
        assert_eq!(serde_json::to_string(&HzOverride::Keep).unwrap(), r#""keep""#);
        assert_eq!(serde_json::to_string(&HzOverride::Hz(90)).unwrap(), r#"{"hz":90}"#);
    }

    #[test]
    fn unwatched_processes_never_register() {
        let s = WatchState::new(cfg(&["game.exe"]), true);
        assert!(!s.on_process_start("notepad.exe", "", 1));
        assert!(!s.is_any_running());
    }

    #[test]
    fn update_config_prunes_running_and_counts() {
        let s = WatchState::new(cfg(&["a.exe", "b.exe"]), true);
        s.on_process_start("a.exe", "", 1);
        s.on_process_start("b.exe", "", 2);

        // Dropping b.exe from the watch list must retire its running entry and
        // drop its count entirely, not leave a stale key behind.
        s.update_config(cfg(&["a.exe"]));
        let counts = s.get_process_counts();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts["a.exe"], 1);
        assert_eq!(s.running_pids(), vec![1]);

        // Re-adding starts from the real running set, not a resurrected count.
        s.update_config(cfg(&["a.exe", "b.exe"]));
        let counts = s.get_process_counts();
        assert_eq!(counts["a.exe"], 1);
        assert_eq!(counts["b.exe"], 0);

        // Removing the last watched entry leaves nothing running.
        s.update_config(cfg(&[]));
        assert!(!s.is_any_running());
        assert!(s.get_process_counts().is_empty());
    }

    #[test]
    fn disabled_entries_never_run() {
        let mut c = cfg(&["a.exe", "b.exe"]);
        c.disabled_processes = vec!["b.exe".into()];
        let s = WatchState::new(c, true);
        assert!(!s.on_process_start("b.exe", "", 2));
        assert!(!s.is_any_running());

        // Disabling a running entry retires it, so sync_hz drops back down.
        assert!(s.on_process_start("a.exe", "", 1));
        let mut c = cfg(&["a.exe", "b.exe"]);
        c.disabled_processes = vec!["a.exe".into()];
        s.update_config(c);
        assert!(!s.is_any_running());
        assert_eq!(s.get_process_counts().len(), 2);
    }

    #[test]
    fn enabled_is_taken_from_the_caller() {
        assert!(!WatchState::new(cfg(&[]), false).is_enabled());
        assert!(WatchState::new(cfg(&[]), true).is_enabled());
    }

    #[test]
    fn set_enabled_toggles_state() {
        let s = WatchState::new(cfg(&[]), true);
        s.set_enabled(false);
        assert!(!s.is_enabled());
        s.set_enabled(true);
        assert!(s.is_enabled());
    }

    fn with_path(name: &str, path: &str) -> WatchedProcess {
        WatchedProcess::WithPath { name: name.into(), path: path.into() }
    }

    #[test]
    fn keys_are_lowercased_and_include_the_path() {
        assert_eq!(WatchedProcess::Name("Game.EXE".into()).key(), "game.exe");
        assert_eq!(
            with_path("Game.exe", r"C:\Games\Foo").key(),
            r"game.exe|c:\games\foo"
        );
    }

    #[test]
    fn name_entries_match_case_insensitively_on_any_path() {
        let p = WatchedProcess::Name("game.exe".into());
        assert!(p.matches("GAME.EXE", ""));
        assert!(p.matches("game.exe", r"C:\anywhere\game.exe"));
        assert!(!p.matches("game2.exe", ""));
    }

    #[test]
    fn path_entries_require_the_path_fragment() {
        let p = with_path("game.exe", r"C:\Games\Foo");
        assert!(p.matches("Game.exe", r"c:\games\foo\bin\game.exe"));
        // Same name, different install: a second copy must not trigger.
        assert!(!p.matches("game.exe", r"C:\Other\game.exe"));
        // Unknown path (e.g. anti-cheat protected process) does not match.
        assert!(!p.matches("game.exe", ""));
        assert!(!p.matches("other.exe", r"C:\Games\Foo\other.exe"));
    }

    #[test]
    fn watched_process_serializes_untagged() {
        let list: Vec<WatchedProcess> = serde_json::from_str(
            r#"["a.exe", {"name": "b.exe", "path": "C:\\B"}]"#,
        )
        .unwrap();
        assert!(matches!(&list[0], WatchedProcess::Name(n) if n == "a.exe"));
        assert!(matches!(&list[1], WatchedProcess::WithPath { name, path }
            if name == "b.exe" && path == r"C:\B"));

        // Name-only entries stay plain strings so old configs keep their shape.
        let out = serde_json::to_string(&list).unwrap();
        assert_eq!(out, r#"["a.exe",{"name":"b.exe","path":"C:\\B"}]"#);
    }

    #[test]
    fn path_entries_use_their_full_key_for_state_and_disabling() {
        let entry = with_path("game.exe", r"C:\Games\Foo");
        let key = entry.key();
        let c = WatchConfig { watched_processes: vec![entry], ..Default::default() };
        let s = WatchState::new(c.clone(), true);

        assert!(s.on_process_start("game.exe", r"C:\Games\Foo\game.exe", 7));
        assert_eq!(s.get_running(), vec![key.clone()]);
        assert_eq!(s.get_process_counts()[&key], 1);

        // Disabling by the bare name does nothing; the composite key is needed.
        let mut by_name = c.clone();
        by_name.disabled_processes = vec!["game.exe".into()];
        s.update_config(by_name);
        assert!(s.is_any_running());

        let mut by_key = c;
        by_key.disabled_processes = vec![key];
        s.update_config(by_key);
        assert!(!s.is_any_running());
    }

    #[test]
    fn get_running_reports_each_entry_once() {
        let s = WatchState::new(cfg(&["a.exe", "b.exe"]), true);
        s.on_process_start("a.exe", "", 1);
        s.on_process_start("a.exe", "", 2);
        s.on_process_start("b.exe", "", 3);
        let mut running = s.get_running();
        running.sort();
        assert_eq!(running, vec!["a.exe".to_string(), "b.exe".to_string()]);
        let mut pids = s.running_pids();
        pids.sort();
        assert_eq!(pids, vec![1, 2, 3]);
    }

    #[test]
    fn first_matching_entry_wins() {
        // A name-only entry listed first claims the process even if a later
        // path entry would also match.
        let c = WatchConfig {
            watched_processes: vec![
                WatchedProcess::Name("game.exe".into()),
                with_path("game.exe", r"C:\Games"),
            ],
            ..Default::default()
        };
        let s = WatchState::new(c, true);
        s.on_process_start("game.exe", r"C:\Games\game.exe", 1);
        assert_eq!(s.get_running(), vec!["game.exe".to_string()]);
    }

    #[test]
    fn status_monitor_prefers_the_first_enabled_one() {
        let mut c = cfg(&[]);
        assert_eq!(c.status_monitor(), None);
        c.monitors.insert("A".into(), profile(false, 144, 60));
        c.monitors.insert("B".into(), profile(true, 144, 60));
        c.monitors.insert("C".into(), profile(true, 144, 60));
        assert_eq!(c.status_monitor(), Some("B"));
    }

    #[test]
    fn keep_override_never_raises_a_monitor() {
        let mut c = cfg(&["a.exe"]);
        c.monitors.insert("M1".into(), profile(true, 144, 60));
        c.process_overrides
            .insert("a.exe".into(), HashMap::from([("M1".into(), HzOverride::Keep)]));
        assert_eq!(c.hz_targets(&["a.exe".into()]), vec![("M1".into(), 60)]);
    }

    #[test]
    fn running_keys_without_config_entries_fall_back_to_globals() {
        let mut c = cfg(&[]);
        c.monitors.insert("M1".into(), profile(true, 144, 60));
        assert_eq!(c.hz_targets(&["ghost.exe".into()]), vec![("M1".into(), 144)]);
    }

    #[test]
    fn parse_keeps_new_layout_and_ignores_legacy_fields() {
        let json = r#"{
            "watched_processes": [],
            "monitors": { "M1": { "enabled": false, "game_hz": 120, "default_hz": 60 } },
            "monitor_name": "M2",
            "game_hz": 240
        }"#;
        let c = WatchConfig::parse(json).unwrap();
        assert_eq!(c.monitors.len(), 1);
        assert_eq!(c.monitors["M1"], profile(false, 120, 60));
    }

    #[test]
    fn parse_fills_defaults_and_rejects_garbage() {
        let c = WatchConfig::parse(r#"{"watched_processes":["a.exe"]}"#).unwrap();
        assert!(c.monitors.is_empty());
        assert!(c.disabled_processes.is_empty());
        assert!(c.process_overrides.is_empty());
        assert!(WatchConfig::parse("{}").is_err());
        assert!(WatchConfig::parse("not json").is_err());
    }

    #[test]
    fn override_deserialization() {
        let o: HashMap<String, HzOverride> =
            serde_json::from_str(r#"{"M1":"keep","M2":{"hz":90}}"#).unwrap();
        assert_eq!(o["M1"], HzOverride::Keep);
        assert_eq!(o["M2"], HzOverride::Hz(90));
    }
}
