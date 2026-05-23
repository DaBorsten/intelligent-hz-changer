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
    running: Arc<Mutex<HashSet<String>>>,
    pub config: Arc<Mutex<WatchConfig>>,
    process_counts: Arc<Mutex<HashMap<String, u32>>>,
    pub enabled: Arc<AtomicBool>,
    pub needs_rescan: Arc<AtomicBool>,
}

impl WatchState {
    pub fn new(config: WatchConfig) -> Self {
        let counts = config
            .watched_processes
            .iter()
            .map(|p| (p.to_lowercase(), 0u32))
            .collect();
        Self {
            running: Arc::new(Mutex::new(HashSet::new())),
            config: Arc::new(Mutex::new(config)),
            process_counts: Arc::new(Mutex::new(counts)),
            enabled: Arc::new(AtomicBool::new(true)),
            needs_rescan: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Returns true only when the first watched process starts (trigger Hz up).
    pub fn on_process_start(&self, name: &str) -> bool {
        let name_lower = name.to_lowercase();
        let config = self.config.lock().unwrap();
        let is_watched = config
            .watched_processes
            .iter()
            .any(|p| p.to_lowercase() == name_lower);
        drop(config);
        if !is_watched {
            return false;
        }

        let mut counts = self.process_counts.lock().unwrap();
        *counts.entry(name_lower.clone()).or_insert(0) += 1;
        drop(counts);

        let mut running = self.running.lock().unwrap();
        let was_empty = running.is_empty();
        running.insert(name_lower);
        was_empty
    }

    /// Returns true only when the last watched process stops (trigger Hz down).
    pub fn on_process_stop(&self, name: &str) -> bool {
        let name_lower = name.to_lowercase();
        let mut running = self.running.lock().unwrap();
        if running.remove(&name_lower) {
            running.is_empty()
        } else {
            false
        }
    }

    pub fn get_running(&self) -> Vec<String> {
        self.running.lock().unwrap().iter().cloned().collect()
    }

    pub fn get_process_counts(&self) -> HashMap<String, u32> {
        self.process_counts.lock().unwrap().clone()
    }

    /// Atomically replaces config and clears the running set.
    /// Returns true if there were active processes before that are no longer watched — caller should reset Hz.
    pub fn update_config(&self, config: WatchConfig) -> bool {
        let mut counts = self.process_counts.lock().unwrap();
        for p in &config.watched_processes {
            counts.entry(p.to_lowercase()).or_insert(0);
        }
        drop(counts);
        let new_watched: HashSet<String> = config
            .watched_processes
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        *self.config.lock().unwrap() = config;
        let mut running = self.running.lock().unwrap();
        // Only remove processes no longer in the new config — keep still-watched ones active.
        let removed_active: Vec<String> = running
            .iter()
            .filter(|p| !new_watched.contains(*p))
            .cloned()
            .collect();
        for p in &removed_active {
            running.remove(p);
        }
        let had_active = !removed_active.is_empty() && running.is_empty();
        self.needs_rescan.store(true, Ordering::Relaxed);
        had_active
    }
}
