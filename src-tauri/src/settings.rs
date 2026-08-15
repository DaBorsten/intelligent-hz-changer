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
