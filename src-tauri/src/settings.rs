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

const APPROVED_KEY: &str =
    "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

#[cfg(windows)]
fn set_startup_approved(app_name: &str, enable: bool) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    if enable {
        // 02 00 00 00 00 00 00 00 00 00 00 00 = enabled
        let _ = Command::new("reg")
            .args([
                "add",
                APPROVED_KEY,
                "/v",
                app_name,
                "/t",
                "REG_BINARY",
                "/d",
                "02000000000000000000000000",
                "/f",
            ])
            .creation_flags(0x08000000)
            .output();
    } else {
        let _ = Command::new("reg")
            .args(["delete", APPROVED_KEY, "/v", app_name, "/f"])
            .creation_flags(0x08000000)
            .output();
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
