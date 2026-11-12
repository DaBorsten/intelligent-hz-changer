use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme: String,
    pub autostart: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub check_updates: bool,
    /// Whether the Hz watcher is active. Persisted across restarts.
    /// Managed by `set_enabled`/`get_enabled`, not by the settings UI, so it
    /// defaults to `true` when absent and is preserved on settings saves.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Opt-in file logging for diagnosing Hz switches. Off by default.
    #[serde(default)]
    pub debug_logging: bool,
    /// Milliseconds between two scans for newly started watched processes.
    /// `None` until the user picks one: the default then follows the detection
    /// method. Read through `scan_interval_ms()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_interval_ms: Option<u32>,
    /// How Windows reads the process list: `"compat"` (Toolhelp snapshot) or
    /// `"performance"` (direct kernel query, Windows 11 24H2 and later). `None`
    /// until the user picks one: the default is then whatever the system
    /// supports. Ignored on other platforms. Read through `fast_detection()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_detection: Option<String>,
}

const DETECTION_COMPAT: &str = "compat";
const DETECTION_PERFORMANCE: &str = "performance";

/// Default scan interval: the fast query is cheap enough to run twice as often.
pub const fn default_scan_interval_ms(fast: bool) -> u32 {
    if fast {
        1000
    } else {
        2000
    }
}

fn default_detection(fast_available: bool) -> &'static str {
    if fast_available {
        DETECTION_PERFORMANCE
    } else {
        DETECTION_COMPAT
    }
}

/// The watcher-related settings depend on whether this system supports the
/// fast process query (`fast_available`), so every reader passes that in.
impl AppSettings {
    /// Whether the scan uses the fast process query: the user's choice, or
    /// the default, and never on a system that doesn't support it.
    pub fn fast_detection(&self, fast_available: bool) -> bool {
        fast_available
            && self.process_detection.as_deref().unwrap_or(DETECTION_PERFORMANCE)
                == DETECTION_PERFORMANCE
    }

    /// The scan interval in effect. Half a second is only on offer with the
    /// fast query; a full scan that often would cost real CPU time. The upper
    /// bound keeps a hand-edited file from making the watcher never scan.
    pub fn scan_interval_ms(&self, fast_available: bool) -> u32 {
        let fast = self.fast_detection(fast_available);
        self.scan_interval_ms
            .unwrap_or(default_scan_interval_ms(fast))
            .clamp(if fast { 500 } else { 1000 }, 60_000)
    }

    /// For the settings UI: both values filled in with what is in effect.
    pub fn resolved(mut self, fast_available: bool) -> Self {
        let fast = self.fast_detection(fast_available);
        self.scan_interval_ms = Some(self.scan_interval_ms(fast_available));
        self.process_detection =
            Some(if fast { DETECTION_PERFORMANCE } else { DETECTION_COMPAT }.into());
        self
    }

    /// For storing: a value equal to the default is stored as "not chosen", so
    /// it keeps following the default (a Windows upgrade that brings the fast
    /// query then switches to it). The UI always sends both values, so this is
    /// what tells a real choice from an echoed default.
    pub fn normalized(mut self, fast_available: bool) -> Self {
        if self.process_detection.as_deref() == Some(default_detection(fast_available)) {
            self.process_detection = None;
        }
        let fast = self.fast_detection(fast_available);
        if self.scan_interval_ms == Some(default_scan_interval_ms(fast)) {
            self.scan_interval_ms = None;
        }
        self
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            autostart: false,
            start_minimized: false,
            close_to_tray: true,
            check_updates: true,
            enabled: true,
            debug_logging: false,
            scan_interval_ms: None,
            process_detection: None,
        }
    }
}

/// Writes the StartupApproved\Run entry Task Manager reads to show the
/// enabled/disabled state. Uses the registry API directly rather than spawning
/// `reg.exe` — no process spawn, no console flash, and no shell quoting to get
/// wrong.
#[cfg(windows)]
fn set_startup_approved(app_name: &str, enable: bool) {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE,
    };

    let approved_path =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run\0";
    let approved_path_w: Vec<u16> = approved_path.encode_utf16().collect();
    let name_w: Vec<u16> = format!("{}\0", app_name).encode_utf16().collect();

    unsafe {
        // The key may not exist yet on a clean profile, so create-or-open.
        let mut hkey = HKEY::default();
        let res = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(approved_path_w.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        );
        if res.is_err() {
            return;
        }

        if enable {
            // First byte 0x02 = enabled (0x03 = disabled by Task Manager);
            // the remaining 11 bytes are a timestamp Windows tolerates as zero.
            let data: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let _ = RegSetValueExW(
                hkey,
                PCWSTR(name_w.as_ptr()),
                0,
                REG_BINARY,
                Some(&data),
            );
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr()));
        }
        let _ = RegCloseKey(hkey);
    }
}

#[cfg(windows)]
pub fn set_autostart(app_name: &str, exe_path: &str, enable: bool) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    };

    let run_path = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0";
    let run_path_w: Vec<u16> = run_path.encode_utf16().collect();
    let name_w: Vec<u16> = format!("{}\0", app_name).encode_utf16().collect();

    unsafe {
        let mut hrun = HKEY::default();
        let res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(run_path_w.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hrun,
        );
        if res.is_err() {
            return Err("Run-Schlüssel konnte nicht geöffnet werden".into());
        }

        if enable {
            let value_w: Vec<u16> = format!("{}\0", exe_path).encode_utf16().collect();
            let bytes =
                std::slice::from_raw_parts(value_w.as_ptr() as *const u8, value_w.len() * 2);
            let res = RegSetValueExW(hrun, PCWSTR(name_w.as_ptr()), 0, REG_SZ, Some(bytes));
            let _ = RegCloseKey(hrun);
            if res.is_err() {
                return Err(res.to_hresult().message().to_string());
            }
        } else {
            let _ = RegDeleteValueW(hrun, PCWSTR(name_w.as_ptr()));
            let _ = RegCloseKey(hrun);
        }
    }

    // Update StartupApproved\Run via reg.exe — Task Manager reads this to show enabled/disabled state.
    set_startup_approved(app_name, enable);

    Ok(())
}

#[cfg(windows)]
pub fn get_autostart(app_name: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_BINARY,
    };

    let run_path = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0";
    let approved_path =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run\0";
    let run_path_w: Vec<u16> = run_path.encode_utf16().collect();
    let approved_path_w: Vec<u16> = approved_path.encode_utf16().collect();
    let name_w: Vec<u16> = format!("{}\0", app_name).encode_utf16().collect();

    unsafe {
        // Entry must exist in Run key
        let mut hrun = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(run_path_w.as_ptr()), 0, KEY_READ, &mut hrun)
            .is_err()
        {
            return false;
        }
        let in_run =
            RegQueryValueExW(hrun, PCWSTR(name_w.as_ptr()), None, None, None, None).is_ok();
        let _ = RegCloseKey(hrun);
        if !in_run {
            return false;
        }

        // Check StartupApproved: first byte 0x03 = disabled by Task Manager
        let mut happroved = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(approved_path_w.as_ptr()),
            0,
            KEY_READ,
            &mut happroved,
        )
        .is_err()
        {
            return true; // no override → enabled
        }

        let mut data = [0u8; 12];
        let mut data_len = data.len() as u32;
        let mut reg_type = REG_BINARY;
        let res = RegQueryValueExW(
            happroved,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut reg_type),
            Some(data.as_mut_ptr()),
            Some(&mut data_len),
        );
        let _ = RegCloseKey(happroved);

        if res.is_err() {
            return true; // no entry → enabled
        }

        data[0] == 0x02
    }
}

/// Path to the XDG autostart .desktop file GNOME's own "Automatisch
/// ausführen" toggle reads and writes, so both stay in sync.
#[cfg(not(windows))]
fn autostart_desktop_path(app_name: &str) -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME nicht gesetzt".to_string())?;
    Ok(std::path::Path::new(&home)
        .join(".config/autostart")
        .join(format!("{app_name}.desktop")))
}

#[cfg(not(windows))]
pub fn set_autostart(app_name: &str, exe_path: &str, enable: bool) -> Result<(), String> {
    let path = autostart_desktop_path(app_name)?;

    if !enable {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={app_name}\n\
         Exec=\"{exe_path}\"\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    std::fs::write(&path, contents).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn get_autostart(app_name: &str) -> bool {
    match autostart_desktop_path(app_name) {
        Ok(path) => path.exists(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(mode: Option<&str>, interval: Option<u32>) -> AppSettings {
        AppSettings {
            process_detection: mode.map(Into::into),
            scan_interval_ms: interval,
            ..Default::default()
        }
    }

    #[test]
    fn detection_defaults_follow_what_the_system_supports() {
        let s = AppSettings::default();
        assert!(s.fast_detection(true));
        assert_eq!(s.scan_interval_ms(true), 1000);
        assert!(!s.fast_detection(false));
        assert_eq!(s.scan_interval_ms(false), 2000);
    }

    #[test]
    fn explicit_detection_choices_win_where_supported() {
        let compat = detection(Some("compat"), None);
        assert!(!compat.fast_detection(true));
        assert_eq!(compat.scan_interval_ms(true), 2000);

        // Performance on a system without the fast query runs as compat.
        let perf = detection(Some("performance"), Some(500));
        assert!(perf.fast_detection(true));
        assert_eq!(perf.scan_interval_ms(true), 500);
        assert!(!perf.fast_detection(false));
        assert_eq!(perf.scan_interval_ms(false), 1000);
    }

    #[test]
    fn scan_interval_is_clamped() {
        assert_eq!(detection(None, Some(0)).scan_interval_ms(true), 500);
        assert_eq!(detection(Some("compat"), Some(500)).scan_interval_ms(true), 1000);
        assert_eq!(detection(None, Some(u32::MAX)).scan_interval_ms(false), 60_000);
    }

    #[test]
    fn resolved_and_normalized_round_trip_defaults() {
        for available in [true, false] {
            let shown = AppSettings::default().resolved(available);
            assert!(shown.process_detection.is_some() && shown.scan_interval_ms.is_some());
            // Saving what the UI was shown must not turn defaults into choices.
            let stored = shown.normalized(available);
            assert_eq!(stored.process_detection, None);
            assert_eq!(stored.scan_interval_ms, None);
        }

        // Real choices survive, and the interval default is the chosen mode's.
        let stored = detection(Some("compat"), Some(2000)).normalized(true);
        assert_eq!(stored.process_detection.as_deref(), Some("compat"));
        assert_eq!(stored.scan_interval_ms, None);
        let stored = detection(Some("performance"), Some(2000)).normalized(true);
        assert_eq!(stored.process_detection, None);
        assert_eq!(stored.scan_interval_ms, Some(2000));
    }

    #[test]
    fn unset_detection_fields_are_not_written() {
        let json = serde_json::to_string(&AppSettings::default()).unwrap();
        assert!(!json.contains("scan_interval_ms") && !json.contains("process_detection"));
    }

    #[test]
    fn defaults_are_tray_friendly() {
        let s = AppSettings::default();
        assert_eq!(s.theme, "system");
        assert!(!s.autostart);
        assert!(!s.start_minimized);
        assert!(s.close_to_tray);
        assert!(s.check_updates);
        assert!(s.enabled);
        assert!(!s.debug_logging);
    }

    #[test]
    fn older_settings_files_get_defaults_for_new_fields() {
        // Written before `enabled` and `debug_logging` existed: the watcher
        // must come up enabled, logging stays off.
        let s: AppSettings = serde_json::from_str(
            r#"{"theme":"dark","autostart":true,"start_minimized":true,
                "close_to_tray":false,"check_updates":false}"#,
        )
        .unwrap();
        assert_eq!(s.theme, "dark");
        assert!(s.autostart && s.start_minimized);
        assert!(!s.close_to_tray && !s.check_updates);
        assert!(s.enabled);
        assert!(!s.debug_logging);
    }

    #[test]
    fn explicit_values_survive_a_round_trip() {
        let s = AppSettings { enabled: false, debug_logging: true, ..Default::default() };
        let back: AppSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.enabled);
        assert!(back.debug_logging);
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        assert!(serde_json::from_str::<AppSettings>(r#"{"theme":"dark"}"#).is_err());
    }
}
