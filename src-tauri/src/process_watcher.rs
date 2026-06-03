use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorHz {
    pub game_hz: u32,
    pub default_hz: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatchConfig {
    pub watched_processes: Vec<String>,
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
    /// PID → lowercased process name of every currently-running watched instance.
    /// Keyed by PID so multiple instances of the same process are tracked independently
    /// (closing one instance must not reset Hz while another is still running).
    running: Arc<Mutex<HashMap<u32, String>>>,
    pub config: Arc<Mutex<WatchConfig>>,
    process_counts: Arc<Mutex<HashMap<String, u32>>>,
    pub enabled: Arc<AtomicBool>,
    /// Serializes every refresh-rate transition so a "Hz up" and a concurrent
    /// "Hz down" can never apply out of order and leave the display in a state
    /// that contradicts the live `running` set.
    pub hz_lock: Arc<Mutex<()>>,
    /// PIDs that already have a live `watch_exit` thread. Shared so the WMI event
    /// loop and the config-change rescan dedup against the same set and never
    /// spawn two exit-watchers for one PID.
    pub watching: Arc<Mutex<HashSet<u32>>>,
}

impl WatchState {
    pub fn new(config: WatchConfig) -> Self {
        let counts = config
            .watched_processes
            .iter()
            .map(|p| (p.to_lowercase(), 0u32))
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
    /// Idempotent per PID: a PID already tracked (e.g. re-detected after a config
    /// rescan) neither re-counts nor re-triggers.
    pub fn on_process_start(&self, name: &str, pid: u32) -> bool {
        let name_lower = name.to_lowercase();
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let is_watched = config
            .watched_processes
            .iter()
            .any(|p| p.to_lowercase() == name_lower);
        drop(config);
        if !is_watched {
            return false;
        }

        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains_key(&pid) {
            return false;
        }
        let was_empty = running.is_empty();
        running.insert(pid, name_lower.clone());
        drop(running);

        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(name_lower).or_insert(0) += 1;

        was_empty
    }

    /// Returns true only when the last watched instance stops (trigger Hz down).
    pub fn on_process_stop(&self, pid: u32) -> bool {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(name) = running.remove(&pid) {
            let empty = running.is_empty();
            drop(running);
            // Keep the live per-name counter in sync with the running set.
            let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = counts.get_mut(&name) {
                *c = c.saturating_sub(1);
            }
            empty
        } else {
            false
        }
    }

    /// True while at least one watched instance is running. Used to decide the
    /// target Hz and to stop `test_hz` from clobbering a watcher-driven rate.
    pub fn is_any_running(&self) -> bool {
        !self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub fn get_running(&self) -> Vec<String> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let unique: HashSet<String> = running.values().cloned().collect();
        unique.into_iter().collect()
    }

    pub fn get_process_counts(&self) -> HashMap<String, u32> {
        self.process_counts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Atomically replaces config and clears the running set.
    /// Returns true if there were active processes before that are no longer watched — caller should reset Hz.
    pub fn update_config(&self, config: WatchConfig) -> bool {
        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        for p in &config.watched_processes {
            counts.entry(p.to_lowercase()).or_insert(0);
        }
        drop(counts);
        let new_watched: HashSet<String> = config
            .watched_processes
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        // Only remove instances whose process is no longer in the new config —
        // keep still-watched ones active.
        let was_active = !running.is_empty();
        let removed: Vec<String> = running
            .iter()
            .filter(|(_pid, name)| !new_watched.contains(*name))
            .map(|(_pid, name)| name.clone())
            .collect();
        running.retain(|_pid, name| new_watched.contains(name));
        // Caller should reset Hz only if removing unwatched instances left nothing running.
        let had_active = was_active && running.is_empty();
        drop(running);
        // Decrement counters for the instances we just dropped so the live
        // per-name count never drifts above the running set.
        let mut counts = self.process_counts.lock().unwrap_or_else(|e| e.into_inner());
        for name in removed {
            if let Some(c) = counts.get_mut(&name) {
                *c = c.saturating_sub(1);
            }
        }
        had_active
    }
}
