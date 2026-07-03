//! Opt-in file logging for diagnosing Hz-switch issues.
//!
//! A single toggle (settings → debug_logging) flips the global `ENABLED` flag.
//! When off, `log` is a cheap atomic load and returns immediately — no file I/O.
//! ponytail: global flag + append-to-file, no logging framework for one debug log.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
// Serialize writes so lines from concurrent watcher threads don't interleave.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn init(path: PathBuf, enabled: bool) {
    let _ = LOG_PATH.set(path);
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

pub fn log(msg: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let line = format!("{} {}\n", timestamp(), msg);
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(windows)]
fn timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

#[cfg(not(windows))]
fn timestamp() -> String {
    String::new()
}
