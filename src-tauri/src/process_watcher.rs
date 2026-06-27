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
    pub fn new(config: WatchConfig) -> Self {
        let counts = config
            .watched_processes
            .iter()
            .map(|p| (p.key(), 0u32))
            .collect();
        Self {
            running: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(config)),
            process_counts: Arc::new(Mutex::new(counts)),
            enabled: Arc::new(AtomicBool::new(true)),
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

    /// Atomically replaces config and prunes running entries no longer in the new config.
    /// Returns true if removing unwatched entries left the running set empty — caller should reset Hz.
    pub fn update_config(&self, config: WatchConfig) -> bool {
        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        for p in &config.watched_processes {
            counts.entry(p.key()).or_insert(0);
        }
        drop(counts);

        let new_watched: HashSet<String> =
            config.watched_processes.iter().map(|p| p.key()).collect();
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let was_active = !running.is_empty();
        let removed: Vec<String> = running
            .iter()
            .filter(|(_pid, key)| !new_watched.contains(*key))
            .map(|(_pid, key)| key.clone())
            .collect();
        running.retain(|_pid, key| new_watched.contains(key));
        let had_active = was_active && running.is_empty();
        drop(running);

        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        for key in removed {
            if let Some(c) = counts.get_mut(&key) {
                *c = c.saturating_sub(1);
            }
        }
        had_active
    }
}
