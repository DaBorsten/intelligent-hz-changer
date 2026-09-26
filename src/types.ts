/** Mirrors the serde-untagged WatchedProcess enum on the Rust side. */
export type WatchedProcess = string | { name: string; path: string };

export function wpName(wp: WatchedProcess): string {
  return typeof wp === "string" ? wp : wp.name;
}

export function wpKey(wp: WatchedProcess): string {
  return typeof wp === "string"
    ? wp.toLowerCase()
    : `${wp.name.toLowerCase()}|${wp.path.toLowerCase()}`;
}

export interface MonitorInfo {
  device_name: string;
  friendly_name: string;
}

export interface MonitorInfoExtended {
  device_name: string;
  friendly_name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
  is_duplicate: boolean;
  current_hz: number;
  max_hz: number;
}

/** Global settings for one monitor. */
export interface MonitorProfile {
  /** Whether watched games switch this monitor unless they say otherwise. */
  enabled: boolean;
  game_hz: number;
  default_hz: number;
}

/** A game's deviation from a monitor's global setting; absent = global. */
export type HzOverride = "keep" | { hz: number };

export interface WatchConfig {
  watched_processes: WatchedProcess[];
  /** Device name → profile. */
  monitors: Record<string, MonitorProfile>;
  /** `wpKey`s of entries that stay listed but never trigger a switch. */
  disabled_processes: string[];
  /** `wpKey` → device name → override. */
  process_overrides: Record<string, Record<string, HzOverride>>;
}

/** The monitor the header and status view report on. Mirrors
 * `WatchConfig::status_monitor`: first switching monitor in sorted order,
 * else the first configured one. */
export function statusMonitor(config: WatchConfig): string {
  const names = Object.keys(config.monitors).sort();
  return names.find((n) => config.monitors[n].enabled) ?? names[0] ?? "";
}

export interface HzChangedPayload {
  current_hz: number;
  hz_from?: number;
  hz_to?: number;
  reason: string;
  process_name?: string;
  event_type?: "process_start" | "process_stop" | "system";
  /** Device name of the monitor this switch applied to. */
  monitor?: string;
  /** Shared by every monitor switched for the same reason. */
  batch?: number;
  /** Backend sequence number, unique and increasing per app run. */
  id: number;
  /** Backend timestamp (ms since epoch). */
  time: number;
}

/** A refresh-rate switch the backend attempted and the display driver refused. */
export interface HzErrorPayload {
  monitor: string;
  target_hz: number;
  reason: string;
  error: string;
}

/** Whether the platform's display backend can be talked to at all. */
export interface BackendStatus {
  ok: boolean;
  /** Stable identifier translated under `monitor.backendError.*`. */
  code: string;
  /** Untranslated specifics (desktop name, missing binary) to interpolate. */
  detail: string;
}

export interface LogEntry {
  /** Backend id of the event (see `HzChangedPayload.id`). */
  id: number;
  /** Backend timestamp of the event (ms since epoch). */
  time: number;
  timestamp: string;
  message: string;
  hz_from?: number;
  hz_to?: number;
  process_name?: string;
  event_type: "process_start" | "process_stop" | "system";
  monitor?: string;
  batch?: number;
}

export interface HzPoint {
  time: number;
  hz: number;
}
