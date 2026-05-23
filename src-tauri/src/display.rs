use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorInfo {
    pub device_name: String,
    pub friendly_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorInfoExtended {
    pub device_name: String,
    pub friendly_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub is_duplicate: bool,
    pub current_hz: u32,
    pub max_hz: u32,
}

#[cfg(windows)]
mod inner {
    use super::{MonitorInfo, MonitorInfoExtended};
    use serde::Deserialize;
    use std::collections::{BTreeSet, HashMap};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_UPDATEREGISTRY,
        DEVMODE_FIELD_FLAGS, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
    };
    use windows::core::PCWSTR;
    use wmi::{COMLibrary, WMIConnection};

    const DM_DISPLAYFREQUENCY: DEVMODE_FIELD_FLAGS = DEVMODE_FIELD_FLAGS(0x400000);
    const DISPLAY_DEVICE_ACTIVE: u32 = 0x1;
    const DISPLAY_DEVICE_PRIMARY_DEVICE: u32 = 0x4;
    const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 0x00000001;

    fn to_wide(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn from_wide(s: &[u16]) -> String {
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf16_lossy(&s[..end])
    }

    #[derive(Deserialize, Debug)]
    struct WmiMonitorId {
        #[serde(rename = "UserFriendlyName")]
        user_friendly_name: Option<Vec<u16>>,
        #[serde(rename = "InstanceName")]
        instance_name: String,
    }

    fn get_wmi_monitor_names() -> HashMap<String, String> {
        let mut map = HashMap::new();
        let com_lib = COMLibrary::new()
            .unwrap_or_else(|_| unsafe { COMLibrary::assume_initialized() });
        let Ok(wmi_con) = WMIConnection::with_namespace_path("root\\wmi", com_lib) else {
            return map;
        };
        let Ok(results): Result<Vec<WmiMonitorId>, _> = wmi_con.query() else {
            return map;
        };

        for m in results {
            if let Some(name_vec) = m.user_friendly_name {
                let end = name_vec.iter().position(|&c| c == 0).unwrap_or(name_vec.len());
                let name = String::from_utf16_lossy(&name_vec[..end])
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    let base = strip_wmi_suffix(&m.instance_name).to_uppercase();
                    map.insert(base, name);
                }
            }
        }
        map
    }

    fn strip_wmi_suffix(s: &str) -> &str {
        if let Some(pos) = s.rfind('_') {
            if s[pos + 1..].chars().all(|c| c.is_ascii_digit()) {
                return &s[..pos];
            }
        }
        s
    }

    fn device_interface_to_instance_prefix(device_id: &str) -> Option<String> {
        let s = device_id
            .trim_start_matches('\\')
            .trim_start_matches('?')
            .trim_start_matches('\\');
        let parts: Vec<&str> = s.split('#').collect();
        if parts.len() >= 3 {
            Some(format!("{}\\{}\\{}", parts[0], parts[1], parts[2]).to_uppercase())
        } else {
            None
        }
    }

    pub fn enumerate_monitors() -> Vec<MonitorInfo> {
        let wmi_names = get_wmi_monitor_names();
        let mut monitors = Vec::new();
        let mut i = 0u32;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
                break;
            }
            i += 1;

            if dd.StateFlags & DISPLAY_DEVICE_ACTIVE == 0 {
                continue;
            }

            let device_name = from_wide(&dd.DeviceName);
            let device_name_wide = to_wide(&device_name);

            let mut monitor_dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };

            let friendly_name = if unsafe {
                EnumDisplayDevicesW(
                    PCWSTR(device_name_wide.as_ptr()),
                    0,
                    &mut monitor_dd,
                    EDD_GET_DEVICE_INTERFACE_NAME,
                )
            }
            .as_bool()
            {
                let device_id = from_wide(&monitor_dd.DeviceID);
                device_interface_to_instance_prefix(&device_id)
                    .and_then(|prefix| wmi_names.get(&prefix).cloned())
                    .unwrap_or_else(|| from_wide(&monitor_dd.DeviceString))
            } else {
                from_wide(&dd.DeviceString)
            };

            monitors.push(MonitorInfo {
                device_name,
                friendly_name,
            });
        }
        monitors
    }

    pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
        // Determine which adapters carry the primary-device flag from Windows
        let mut primary_names = std::collections::HashSet::new();
        {
            let mut i = 0u32;
            loop {
                let mut dd = DISPLAY_DEVICEW {
                    cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                    ..Default::default()
                };
                if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
                    break;
                }
                i += 1;
                if dd.StateFlags & DISPLAY_DEVICE_ACTIVE != 0
                    && dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0
                {
                    primary_names.insert(from_wide(&dd.DeviceName));
                }
            }
        }

        let basic = enumerate_monitors();
        let mut result = Vec::new();

        for mon in basic {
            let wide = to_wide(&mon.device_name);
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if !unsafe {
                EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
            }
            .as_bool()
            {
                continue;
            }

            let pos = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
            let x = pos.x;
            let y = pos.y;
            let width = dm.dmPelsWidth;
            let height = dm.dmPelsHeight;
            let current_hz = dm.dmDisplayFrequency;
            let is_primary = primary_names.contains(&mon.device_name);

            let max_hz = {
                let mut rates = BTreeSet::new();
                let mut idx = 0u32;
                loop {
                    let mut dm2 = DEVMODEW {
                        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                        ..Default::default()
                    };
                    if !unsafe {
                        EnumDisplaySettingsW(
                            PCWSTR(wide.as_ptr()),
                            ENUM_DISPLAY_SETTINGS_MODE(idx),
                            &mut dm2,
                        )
                    }
                    .as_bool()
                    {
                        break;
                    }
                    if dm2.dmPelsWidth == width && dm2.dmPelsHeight == height {
                        rates.insert(dm2.dmDisplayFrequency);
                    }
                    idx += 1;
                }
                rates.into_iter().max().unwrap_or(current_hz)
            };

            result.push(MonitorInfoExtended {
                device_name: mon.device_name,
                friendly_name: mon.friendly_name,
                x,
                y,
                width,
                height,
                is_primary,
                is_duplicate: false,
                current_hz,
                max_hz,
            });
        }

        // Mark monitors that share the same position+size as duplicates (clone/mirror mode)
        let mut pos_count: HashMap<(i32, i32, u32, u32), usize> = HashMap::new();
        for m in &result {
            *pos_count.entry((m.x, m.y, m.width, m.height)).or_insert(0) += 1;
        }
        for m in &mut result {
            m.is_duplicate = pos_count[&(m.x, m.y, m.width, m.height)] > 1;
        }

        result
    }

    pub fn get_supported_refresh_rates(monitor_name: &str) -> Vec<u32> {
        let wide = to_wide(monitor_name);
        let mut current = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !unsafe {
            EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut current)
        }
        .as_bool()
        {
            return vec![];
        }
        let cw = current.dmPelsWidth;
        let ch = current.dmPelsHeight;

        let mut rates = BTreeSet::new();
        let mut idx = 0u32;
        loop {
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if !unsafe {
                EnumDisplaySettingsW(
                    PCWSTR(wide.as_ptr()),
                    ENUM_DISPLAY_SETTINGS_MODE(idx),
                    &mut dm,
                )
            }
            .as_bool()
            {
                break;
            }
            if dm.dmPelsWidth == cw && dm.dmPelsHeight == ch {
                rates.insert(dm.dmDisplayFrequency);
            }
            idx += 1;
        }
        rates.into_iter().collect()
    }

    pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
        let wide = to_wide(monitor_name);
        let mut dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if unsafe {
            EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
        }
        .as_bool()
        {
            dm.dmDisplayFrequency
        } else {
            0
        }
    }

    pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
        let wide = to_wide(monitor_name);
        let mut dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !unsafe {
            EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
        }
        .as_bool()
        {
            return Err("EnumDisplaySettingsW failed".into());
        }
        dm.dmDisplayFrequency = hz;
        dm.dmFields |= DM_DISPLAYFREQUENCY;
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(wide.as_ptr()),
                Some(&dm as *const DEVMODEW),
                HWND(std::ptr::null_mut()),
                CDS_UPDATEREGISTRY,
                None,
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("ChangeDisplaySettingsExW failed: {}", result.0))
        }
    }
}

pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    #[cfg(windows)]
    return inner::enumerate_monitors();
    #[cfg(not(windows))]
    vec![]
}

pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
    #[cfg(windows)]
    return inner::get_monitors_extended();
    #[cfg(not(windows))]
    vec![]
}

pub fn get_supported_refresh_rates(monitor_name: &str) -> Vec<u32> {
    #[cfg(windows)]
    return inner::get_supported_refresh_rates(monitor_name);
    #[cfg(not(windows))]
    {
        let _ = monitor_name;
        vec![]
    }
}

pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
    #[cfg(windows)]
    return inner::get_current_refresh_rate(monitor_name);
    #[cfg(not(windows))]
    {
        let _ = monitor_name;
        0
    }
}

pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
    #[cfg(windows)]
    return inner::set_refresh_rate(monitor_name, hz);
    #[cfg(not(windows))]
    {
        let _ = (monitor_name, hz);
        Err("Not supported on this platform".into())
    }
}
