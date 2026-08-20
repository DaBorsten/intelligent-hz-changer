use super::{BackendStatus, MonitorInfo, MonitorInfoExtended};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq)]
enum Backend {
    /// GNOME on Wayland or X11 — Mutter exposes the same D-Bus config API
    /// either way.
    Gnome,
    /// KDE Plasma on Wayland or X11 — via the `kscreen-doctor` CLI that
    /// ships with the `kscreen` package on essentially every install.
    Kde,
    /// Any other X11 desktop.
    Xrandr,
}

/// Picks a backend once per process rather than compile time (there's no
/// single API that covers GNOME, KDE and everything-else-on-X11) or on every
/// call (each candidate's reachability check is itself a D-Bus round trip or
/// a spawned process — the desktop environment doesn't change mid-session).
fn backend() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(|| {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        if desktop.contains("gnome") && gnome::is_reachable() {
            Backend::Gnome
        } else if desktop.contains("kde") && kde::is_reachable() {
            Backend::Kde
        } else {
            Backend::Xrandr
        }
    })
}

/// True for a native Wayland session, where `xrandr` — the fallback backend —
/// cannot see or change anything.
fn is_wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|s| s.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Why the display backend can't work, if it can't.
///
/// Only the `xrandr` fallback can land in an unusable state: GNOME and KDE are
/// picked exclusively after their own reachability check has already succeeded,
/// so reaching this function on either means the desktop was detected but its
/// interface wasn't there.
pub fn backend_status() -> BackendStatus {
    if backend() != Backend::Xrandr {
        return BackendStatus::working();
    }

    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if is_wayland_session() {
        // On Plasma the fallback is only reached because `kscreen-doctor` is
        // missing, and installing it is a fix the user can actually act on —
        // worth saying instead of the generic "unsupported session".
        if desktop.to_lowercase().contains("kde") {
            return BackendStatus::failing("kde_tool_missing", "kscreen-doctor");
        }
        return BackendStatus::failing(
            "wayland_unsupported",
            if desktop.is_empty() { "Wayland".to_string() } else { desktop },
        );
    }

    if !xrandr::is_available() {
        return BackendStatus::failing("xrandr_missing", "xrandr");
    }
    if xrandr::enumerate_monitors().is_empty() {
        return BackendStatus::failing("no_monitors", "xrandr");
    }
    BackendStatus::working()
}

pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    match backend() {
        Backend::Gnome => gnome::enumerate_monitors(),
        Backend::Kde => kde::enumerate_monitors(),
        Backend::Xrandr => xrandr::enumerate_monitors(),
    }
}

pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
    match backend() {
        Backend::Gnome => gnome::get_monitors_extended(),
        Backend::Kde => kde::get_monitors_extended(),
        Backend::Xrandr => xrandr::get_monitors_extended(),
    }
}

pub fn get_supported_refresh_rates(monitor_name: &str) -> Vec<u32> {
    match backend() {
        Backend::Gnome => gnome::get_supported_refresh_rates(monitor_name),
        Backend::Kde => kde::get_supported_refresh_rates(monitor_name),
        Backend::Xrandr => xrandr::get_supported_refresh_rates(monitor_name),
    }
}

pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
    match backend() {
        Backend::Gnome => gnome::get_current_refresh_rate(monitor_name),
        Backend::Kde => kde::get_current_refresh_rate(monitor_name),
        Backend::Xrandr => xrandr::get_current_refresh_rate(monitor_name),
    }
}

pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
    match backend() {
        Backend::Gnome => gnome::set_refresh_rate(monitor_name, hz),
        Backend::Kde => kde::set_refresh_rate(monitor_name, hz),
        Backend::Xrandr => xrandr::set_refresh_rate(monitor_name, hz),
    }
}

/// How long a trial rate stays applied. On GNOME that's Mutter's own
/// confirmation timeout, which we can't shorten; everywhere else we run the
/// countdown ourselves and keep the shorter default.
pub fn test_seconds() -> u32 {
    match backend() {
        Backend::Gnome => gnome::CONFIRMATION_SECONDS,
        _ => super::DEFAULT_TEST_SECONDS,
    }
}

/// Applies `hz` as a trial. `Ok(true)` means the desktop environment took the
/// trial over — it puts up its own keep/revert prompt and restores the previous
/// rate when the user declines or lets it time out, so the caller must neither
/// revert nor dismiss anything itself.
pub fn set_refresh_rate_for_test(monitor_name: &str, hz: u32) -> Result<bool, String> {
    match backend() {
        Backend::Gnome => gnome::set_refresh_rate_confirmed(monitor_name, hz).map(|()| true),
        _ => set_refresh_rate(monitor_name, hz).map(|()| false),
    }
}

/// GNOME backend: talks to Mutter's `org.gnome.Mutter.DisplayConfig` over the
/// session D-Bus. Covers GNOME on both Wayland and X11 (Mutter is the
/// compositor/WM in both cases and exposes the same interface either way).
mod gnome {
    use super::{MonitorInfo, MonitorInfoExtended};
    use std::collections::{BTreeSet, HashMap};
    use zbus::zvariant::OwnedValue;

    const BUS_NAME: &str = "org.gnome.Mutter.DisplayConfig";
    const OBJECT_PATH: &str = "/org/gnome/Mutter/DisplayConfig";
    const INTERFACE: &str = "org.gnome.Mutter.DisplayConfig";

    /// `ApplyMonitorsConfig` methods. `Temporary` applies the mode and nothing
    /// else; `Persistent` additionally makes Mutter ask gnome-shell to prompt
    /// the user ("Keep these display settings?") and roll the mode back unless
    /// they confirm — only then is it written to `~/.config/monitors.xml`.
    const METHOD_TEMPORARY: u32 = 1;
    const METHOD_PERSISTENT: u32 = 2;

    /// How long gnome-shell's prompt counts down before reverting. Hard-coded
    /// in Mutter (`meta_monitor_manager_get_display_configuration_timeout`),
    /// with no setting to change or suppress it.
    pub const CONFIRMATION_SECONDS: u32 = 20;

    type MonitorSpec = (String, String, String, String); // connector, vendor, product, serial
    type ModeInfo = (
        String,             // mode id
        i32,                // width
        i32,                // height
        f64,                // refresh rate
        f64,                // preferred scale
        Vec<f64>,           // supported scales
        HashMap<String, OwnedValue>, // mode properties (is-current, is-preferred)
    );
    type MonitorEntry = (MonitorSpec, Vec<ModeInfo>, HashMap<String, OwnedValue>);
    type LogicalMonitorEntry = (
        i32,             // x
        i32,             // y
        f64,             // scale
        u32,             // transform
        bool,            // is_primary
        Vec<MonitorSpec>,
        HashMap<String, OwnedValue>,
    );
    type CurrentState = (
        u32, // serial
        Vec<MonitorEntry>,
        Vec<LogicalMonitorEntry>,
        HashMap<String, OwnedValue>,
    );

    fn connection() -> Option<zbus::blocking::Connection> {
        zbus::blocking::Connection::session().ok()
    }

    pub fn is_reachable() -> bool {
        get_state().is_some()
    }

    fn get_state() -> Option<CurrentState> {
        let conn = connection()?;
        let reply = conn
            .call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), "GetCurrentState", &())
            .ok()?;
        reply.body().deserialize::<CurrentState>().ok()
    }

    fn friendly_name(spec: &MonitorSpec, props: &HashMap<String, OwnedValue>) -> String {
        props
            .get("display-name")
            .and_then(|v| String::try_from(v.clone()).ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let (connector, vendor, product, _) = spec;
                if vendor != "unknown" && !vendor.is_empty() {
                    format!("{vendor} {product}")
                } else {
                    connector.clone()
                }
            })
    }

    fn is_current_mode(props: &HashMap<String, OwnedValue>) -> bool {
        props
            .get("is-current")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false)
    }

    pub fn enumerate_monitors() -> Vec<MonitorInfo> {
        let Some((_, monitors, ..)) = get_state() else { return vec![] };
        monitors
            .into_iter()
            .map(|(spec, _, props)| MonitorInfo {
                device_name: spec.0.clone(),
                friendly_name: friendly_name(&spec, &props),
            })
            .collect()
    }

    pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
        let Some((_, monitors, logical_monitors, _)) = get_state() else { return vec![] };

        let mut result = Vec::new();
        for (spec, modes, props) in &monitors {
            let connector = &spec.0;
            let logical = logical_monitors
                .iter()
                .find(|(.., mons, _)| mons.iter().any(|m| &m.0 == connector));
            let Some((x, y, _scale, _transform, is_primary, ..)) = logical else { continue };

            let Some((_, width, height, rate, ..)) =
                modes.iter().find(|(_, _, _, _, _, _, p)| is_current_mode(p))
            else {
                continue;
            };
            let current_hz = rate.round() as u32;

            let max_hz = modes
                .iter()
                .filter(|(_, w, h, ..)| *w == *width && *h == *height)
                .map(|(_, _, _, r, ..)| r.round() as u32)
                .max()
                .unwrap_or(current_hz);

            result.push(MonitorInfoExtended {
                device_name: connector.clone(),
                friendly_name: friendly_name(spec, props),
                x: *x,
                y: *y,
                width: *width as u32,
                height: *height as u32,
                is_primary: *is_primary,
                is_duplicate: false,
                current_hz,
                max_hz,
            });
        }

        let mut pos_count: HashMap<(i32, i32, u32, u32), usize> = HashMap::new();
        for m in &result {
            *pos_count.entry((m.x, m.y, m.width, m.height)).or_insert(0) += 1;
        }
        for m in &mut result {
            m.is_duplicate = pos_count[&(m.x, m.y, m.width, m.height)] > 1;
        }

        result
    }

    fn modes_for(monitor_name: &str) -> Option<(Vec<ModeInfo>, i32, i32)> {
        let (_, monitors, ..) = get_state()?;
        let (_, modes, _) = monitors.into_iter().find(|(spec, ..)| spec.0 == monitor_name)?;
        let (_, w, h, ..) = modes.iter().find(|(_, _, _, _, _, _, p)| is_current_mode(p))?;
        let (w, h) = (*w, *h);
        Some((modes, w, h))
    }

    pub fn get_supported_refresh_rates(monitor_name: &str) -> Vec<u32> {
        let Some((modes, w, h)) = modes_for(monitor_name) else { return vec![] };
        modes
            .iter()
            .filter(|(_, mw, mh, ..)| *mw == w && *mh == h)
            .map(|(_, _, _, r, ..)| r.round() as u32)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
        let Some((modes, w, h)) = modes_for(monitor_name) else { return 0 };
        modes
            .iter()
            .filter(|(_, mw, mh, ..)| *mw == w && *mh == h)
            .find(|(_, _, _, _, _, _, p)| is_current_mode(p))
            .map(|(_, _, _, r, ..)| r.round() as u32)
            .unwrap_or(0)
    }

    /// Silent switch, as the watcher does it on process start/stop: no prompt,
    /// and nothing written to the user's saved monitor configuration.
    pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
        apply(monitor_name, hz, METHOD_TEMPORARY)
    }

    /// Switch as a trial: gnome-shell shows its keep/revert prompt and undoes
    /// the change after [`CONFIRMATION_SECONDS`] unless the user keeps it. We
    /// deliberately don't call `ConfirmConfiguration` — the whole point of the
    /// prompt here is that the user decides.
    pub fn set_refresh_rate_confirmed(monitor_name: &str, hz: u32) -> Result<(), String> {
        apply(monitor_name, hz, METHOD_PERSISTENT)
    }

    fn apply(monitor_name: &str, hz: u32, method: u32) -> Result<(), String> {
        let conn = connection().ok_or("no D-Bus session connection")?;
        let (serial, monitors, logical_monitors, top_props) =
            get_state().ok_or("GetCurrentState failed")?;

        let (_, target_modes, _) = monitors
            .iter()
            .find(|(spec, ..)| spec.0 == monitor_name)
            .ok_or_else(|| format!("unknown monitor '{monitor_name}'"))?;
        let (_, w, h, ..) = target_modes
            .iter()
            .find(|(_, _, _, _, _, _, p)| is_current_mode(p))
            .ok_or("monitor has no current mode")?;
        let (w, h) = (*w, *h);

        // Prefer the mode whose rounded rate matches exactly; if several modes
        // round to the same Hz (e.g. 59.93 and 59.88 near 60), pick the one
        // closest to the integer target.
        let target_mode_id = target_modes
            .iter()
            .filter(|(_, mw, mh, ..)| *mw == w && *mh == h)
            .min_by(|(_, _, _, r1, ..), (_, _, _, r2, ..)| {
                let d1 = (r1.round() as i64 - hz as i64).abs();
                let d2 = (r2.round() as i64 - hz as i64).abs();
                d1.cmp(&d2)
            })
            .filter(|(_, _, _, r, ..)| r.round() as u32 == hz)
            .map(|(id, ..)| id.clone())
            .ok_or_else(|| format!("{hz} Hz not supported at {w}x{h} on '{monitor_name}'"))?;

        let logical_config: Vec<(i32, i32, f64, u32, bool, Vec<(String, String, HashMap<String, OwnedValue>)>)> =
            logical_monitors
                .into_iter()
                .map(|(x, y, scale, transform, is_primary, mons, _)| {
                    let mon_configs = mons
                        .into_iter()
                        .map(|spec| {
                            let connector = spec.0.clone();
                            let mode_id = if connector == monitor_name {
                                target_mode_id.clone()
                            } else {
                                // Keep whatever mode this (mirrored) monitor is
                                // currently on.
                                monitors
                                    .iter()
                                    .find(|(s, ..)| s.0 == connector)
                                    .and_then(|(_, modes, _)| {
                                        modes.iter().find(|(_, _, _, _, _, _, p)| is_current_mode(p))
                                    })
                                    .map(|(id, ..)| id.clone())
                                    .unwrap_or_default()
                            };
                            (connector, mode_id, HashMap::new())
                        })
                        .collect();
                    (x, y, scale, transform, is_primary, mon_configs)
                })
                .collect();

        let mut properties = HashMap::new();
        if let Some(layout_mode) = top_props.get("layout-mode") {
            properties.insert("layout-mode", layout_mode.clone());
        }

        conn.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(INTERFACE),
            "ApplyMonitorsConfig",
            &(serial, method, logical_config, properties),
        )
        .map_err(|e| format!("ApplyMonitorsConfig failed: {e}"))?;

        Ok(())
    }
}

/// KDE Plasma backend (Wayland or X11): shells out to `kscreen-doctor`, which
/// ships with the `kscreen` package on essentially every Plasma install and
/// is KWin's own CLI for the same output configuration KDE's display settings
/// use. Unlike GNOME there's no stable public D-Bus API for this on KWin, so
/// the CLI is the supported integration point.
///
/// NOTE: developed and unit-tested against the documented `kscreen-doctor
/// --json` schema and mode-set syntax, but not run against a live Plasma
/// session (none was available while building this) — please report back if
/// field names or behavior have drifted on your KDE version.
mod kde {
    use super::{MonitorInfo, MonitorInfoExtended};
    use serde::Deserialize;
    use std::collections::{BTreeSet, HashMap};
    use std::process::Command;

    #[derive(Deserialize)]
    struct KscreenState {
        #[serde(default)]
        outputs: Vec<KscreenOutput>,
    }

    #[derive(Deserialize)]
    struct KscreenOutput {
        name: String,
        #[serde(default)]
        connected: bool,
        #[serde(default)]
        enabled: bool,
        /// Lower is higher priority; the primary output is priority 1.
        #[serde(default)]
        priority: Option<u32>,
        #[serde(default)]
        pos: Option<KscreenPos>,
        #[serde(rename = "currentModeId", default)]
        current_mode_id: Option<String>,
        #[serde(default)]
        modes: Vec<KscreenMode>,
    }

    #[derive(Deserialize)]
    struct KscreenPos {
        x: i32,
        y: i32,
    }

    #[derive(Deserialize)]
    struct KscreenMode {
        id: String,
        size: KscreenSize,
        #[serde(rename = "refreshRate")]
        refresh_rate: f64,
    }

    #[derive(Deserialize)]
    struct KscreenSize {
        width: i32,
        height: i32,
    }

    fn get_state() -> Option<KscreenState> {
        let out = Command::new("kscreen-doctor").arg("--json").output().ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    pub fn is_reachable() -> bool {
        get_state().is_some_and(|s| !s.outputs.is_empty())
    }

    fn active_outputs(state: &KscreenState) -> impl Iterator<Item = &KscreenOutput> {
        state.outputs.iter().filter(|o| o.connected && o.enabled)
    }

    fn current_mode(o: &KscreenOutput) -> Option<&KscreenMode> {
        let id = o.current_mode_id.as_ref()?;
        o.modes.iter().find(|m| &m.id == id)
    }

    pub fn enumerate_monitors() -> Vec<MonitorInfo> {
        let Some(state) = get_state() else { return vec![] };
        active_outputs(&state)
            .map(|o| MonitorInfo { device_name: o.name.clone(), friendly_name: o.name.clone() })
            .collect()
    }

    pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
        let Some(state) = get_state() else { return vec![] };
        let mut result = Vec::new();
        for o in active_outputs(&state) {
            let Some(mode) = current_mode(o) else { continue };
            let pos = o.pos.as_ref();
            let max_hz = o
                .modes
                .iter()
                .filter(|m| m.size.width == mode.size.width && m.size.height == mode.size.height)
                .map(|m| m.refresh_rate.round() as u32)
                .max()
                .unwrap_or_else(|| mode.refresh_rate.round() as u32);

            result.push(MonitorInfoExtended {
                device_name: o.name.clone(),
                friendly_name: o.name.clone(),
                x: pos.map(|p| p.x).unwrap_or(0),
                y: pos.map(|p| p.y).unwrap_or(0),
                width: mode.size.width as u32,
                height: mode.size.height as u32,
                is_primary: o.priority == Some(1),
                is_duplicate: false,
                current_hz: mode.refresh_rate.round() as u32,
                max_hz,
            });
        }

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
        let Some(state) = get_state() else { return vec![] };
        let Some(o) = state.outputs.iter().find(|o| o.name == monitor_name) else { return vec![] };
        let Some(mode) = current_mode(o) else { return vec![] };
        o.modes
            .iter()
            .filter(|m| m.size.width == mode.size.width && m.size.height == mode.size.height)
            .map(|m| m.refresh_rate.round() as u32)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
        let Some(state) = get_state() else { return 0 };
        state
            .outputs
            .iter()
            .find(|o| o.name == monitor_name)
            .and_then(current_mode)
            .map(|m| m.refresh_rate.round() as u32)
            .unwrap_or(0)
    }

    pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
        let state = get_state().ok_or("kscreen-doctor --json failed")?;
        let o = state
            .outputs
            .iter()
            .find(|o| o.name == monitor_name)
            .ok_or_else(|| format!("unknown monitor '{monitor_name}'"))?;
        let mode = current_mode(o).ok_or("monitor has no current mode")?;
        let (w, h) = (mode.size.width, mode.size.height);

        let target_id = o
            .modes
            .iter()
            .filter(|m| m.size.width == w && m.size.height == h)
            .min_by(|m1, m2| {
                let d1 = (m1.refresh_rate.round() as i64 - hz as i64).abs();
                let d2 = (m2.refresh_rate.round() as i64 - hz as i64).abs();
                d1.cmp(&d2)
            })
            .filter(|m| m.refresh_rate.round() as u32 == hz)
            .map(|m| m.id.clone())
            .ok_or_else(|| format!("{hz} Hz not supported at {w}x{h} on '{monitor_name}'"))?;

        let status = Command::new("kscreen-doctor")
            .arg(format!("output.{monitor_name}.mode.{target_id}"))
            .status()
            .map_err(|e| format!("failed to run kscreen-doctor: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("kscreen-doctor exited with status {status}"))
        }
    }
}

/// X11 backend (any window manager): shells out to `xrandr`, which is present
/// on essentially every X11 desktop. Not usable under a native Wayland session.
mod xrandr {
    use super::{MonitorInfo, MonitorInfoExtended};
    use std::collections::{BTreeSet, HashMap};
    use std::process::Command;

    struct Mode {
        width: u32,
        height: u32,
        rate: f64,
        current: bool,
    }

    struct Output {
        name: String,
        connected: bool,
        primary: bool,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        has_geometry: bool,
        modes: Vec<Mode>,
    }

    fn query() -> Option<Vec<Output>> {
        let out = Command::new("xrandr").arg("--query").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Some(parse_query(&text))
    }

    fn parse_query(text: &str) -> Vec<Output> {
        let mut outputs = Vec::new();
        let mut current: Option<Output> = None;

        for line in text.lines() {
            if !line.starts_with(char::is_whitespace) {
                if let Some(o) = current.take() {
                    outputs.push(o);
                }
                if line.starts_with("Screen") {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let Some(name) = parts.next() else { continue };
                let rest: Vec<&str> = parts.collect();
                let connected = rest.first() == Some(&"connected");
                if !connected && rest.first() != Some(&"disconnected") {
                    continue;
                }
                let primary = rest.contains(&"primary");
                let geom = rest.iter().find(|t| {
                    t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) && t.contains('+')
                });
                let (x, y, width, height, has_geometry) = geom
                    .and_then(|g| parse_geometry(g))
                    .map(|(w, h, x, y)| (x, y, w, h, true))
                    .unwrap_or((0, 0, 0, 0, false));
                current = Some(Output {
                    name: name.to_string(),
                    connected,
                    primary,
                    x,
                    y,
                    width,
                    height,
                    has_geometry,
                    modes: Vec::new(),
                });
            } else if let Some(o) = current.as_mut() {
                let mut tokens = line.split_whitespace();
                let Some(res) = tokens.next() else { continue };
                let Some((w, h)) = res.split_once('x').and_then(|(a, b)| {
                    Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))
                }) else {
                    continue;
                };
                for tok in tokens {
                    let current_flag = tok.contains('*');
                    let cleaned = tok.trim_end_matches(['*', '+']);
                    if let Ok(rate) = cleaned.parse::<f64>() {
                        o.modes.push(Mode { width: w, height: h, rate, current: current_flag });
                    }
                }
            }
        }
        if let Some(o) = current.take() {
            outputs.push(o);
        }
        outputs
    }

    /// Parses `1920x1080+0+0` into `(width, height, x, y)`.
    fn parse_geometry(g: &str) -> Option<(u32, u32, i32, i32)> {
        let (res, rest) = g.split_once('+')?;
        let (x, y) = rest.split_once('+')?;
        let (w, h) = res.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?, x.parse().ok()?, y.parse().ok()?))
    }

    /// Whether `xrandr` is installed and can talk to a server at all.
    pub fn is_available() -> bool {
        query().is_some()
    }

    pub fn enumerate_monitors() -> Vec<MonitorInfo> {
        let Some(outputs) = query() else { return vec![] };
        outputs
            .into_iter()
            .filter(|o| o.connected && o.has_geometry)
            .map(|o| MonitorInfo { device_name: o.name.clone(), friendly_name: o.name })
            .collect()
    }

    pub fn get_monitors_extended() -> Vec<MonitorInfoExtended> {
        let Some(outputs) = query() else { return vec![] };
        let mut result = Vec::new();
        for o in outputs.into_iter().filter(|o| o.connected && o.has_geometry) {
            let current_hz = o
                .modes
                .iter()
                .find(|m| m.current)
                .map(|m| m.rate.round() as u32)
                .unwrap_or(0);
            let max_hz = o
                .modes
                .iter()
                .filter(|m| m.width == o.width && m.height == o.height)
                .map(|m| m.rate.round() as u32)
                .max()
                .unwrap_or(current_hz);
            result.push(MonitorInfoExtended {
                device_name: o.name,
                friendly_name: String::new(),
                x: o.x,
                y: o.y,
                width: o.width,
                height: o.height,
                is_primary: o.primary,
                is_duplicate: false,
                current_hz,
                max_hz,
            });
        }
        for m in &mut result {
            if m.friendly_name.is_empty() {
                m.friendly_name = m.device_name.clone();
            }
        }
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
        let Some(outputs) = query() else { return vec![] };
        let Some(o) = outputs.into_iter().find(|o| o.name == monitor_name) else { return vec![] };
        o.modes
            .iter()
            .filter(|m| m.width == o.width && m.height == o.height)
            .map(|m| m.rate.round() as u32)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn get_current_refresh_rate(monitor_name: &str) -> u32 {
        let Some(outputs) = query() else { return 0 };
        outputs
            .into_iter()
            .find(|o| o.name == monitor_name)
            .and_then(|o| o.modes.into_iter().find(|m| m.current).map(|m| m.rate.round() as u32))
            .unwrap_or(0)
    }

    pub fn set_refresh_rate(monitor_name: &str, hz: u32) -> Result<(), String> {
        let outputs = query().ok_or("xrandr --query failed")?;
        let o = outputs
            .into_iter()
            .find(|o| o.name == monitor_name)
            .ok_or_else(|| format!("unknown monitor '{monitor_name}'"))?;
        let matches = o
            .modes
            .iter()
            .any(|m| m.width == o.width && m.height == o.height && m.rate.round() as u32 == hz);
        if !matches {
            return Err(format!("{hz} Hz not supported at {}x{} on '{monitor_name}'", o.width, o.height));
        }

        let status = Command::new("xrandr")
            .args([
                "--output",
                monitor_name,
                "--mode",
                &format!("{}x{}", o.width, o.height),
                "--rate",
                &hz.to_string(),
            ])
            .status()
            .map_err(|e| format!("failed to run xrandr: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("xrandr exited with status {status}"))
        }
    }
}
