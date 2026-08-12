use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Resolves an exe path from a PID via QueryFullProcessImageNameW.
/// WMI returns null ExecutablePath for Vanguard/EAC-protected processes — this succeeds where WMI fails.
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorHz {
    pub game_hz: u32,
    pub default_hz: u32,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatchConfig {
    pub watched_processes: Vec<WatchedProcess>,
    pub monitor_name: String,
    pub game_hz: u32,
    pub default_hz: u32,
    #[serde(default)]
    pub monitor_settings: HashMap<String, MonitorHz>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            watched_processes: vec![],
            monitor_name: String::new(),
            game_hz: 144,
            default_hz: 60,
            monitor_settings: HashMap::new(),
        }
    }
}

impl WatchConfig {
    pub fn game_hz_for(&self, monitor: &str) -> u32 {
        self.monitor_settings.get(monitor).map(|s| s.game_hz).unwrap_or(self.game_hz)
    }
    pub fn default_hz_for(&self, monitor: &str) -> u32 {
        self.monitor_settings.get(monitor).map(|s| s.default_hz).unwrap_or(self.default_hz)
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

    /// Returns true only when the first watched process starts (trigger Hz up).
    /// Idempotent per PID.
    pub fn on_process_start(&self, name: &str, exe_path: &str, pid: u32) -> bool {
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let matched = config.watched_processes.iter().find(|p| p.matches(name, exe_path));
        let Some(entry) = matched else { return false };
        let key = entry.key();
        drop(config);

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains_key(&pid) {
            return false;
        }
        let was_empty = running.is_empty();
        running.insert(pid, key.clone());
        drop(running);

        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(key).or_insert(0) += 1;

        was_empty
    }

    /// Returns true only when the last watched instance stops (trigger Hz down).
    pub fn on_process_stop(&self, pid: u32) -> bool {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = running.remove(&pid) {
            let empty = running.is_empty();
            drop(running);
            let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = counts.get_mut(&key) {
                *c = c.saturating_sub(1);
            }
            empty
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
    /// new config. Callers re-derive the target Hz via `sync_hz` afterwards, so
    /// nothing needs to be returned here.
    pub fn update_config(&self, config: WatchConfig) {
        let new_watched: HashSet<String> =
            config.watched_processes.iter().map(|p| p.key()).collect();
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|_pid, key| new_watched.contains(key));
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
    fn enabled_is_taken_from_the_caller() {
        assert!(!WatchState::new(cfg(&[]), false).is_enabled());
        assert!(WatchState::new(cfg(&[]), true).is_enabled());
    }
}
