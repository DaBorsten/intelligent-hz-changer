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

export interface MonitorHz {
  game_hz: number;
  default_hz: number;
}

export interface WatchConfig {
  watched_processes: WatchedProcess[];
  monitor_name: string;
  game_hz: number;
  default_hz: number;
  monitor_settings: Record<string, MonitorHz>;
}

export interface HzChangedPayload {
  current_hz: number;
  hz_from?: number;
  hz_to?: number;
  reason: string;
  process_name?: string;
  event_type?: "process_start" | "process_stop" | "system";
}

export interface LogEntry {
  id: number;
  timestamp: string;
  message: string;
  hz_from?: number;
  hz_to?: number;
  process_name?: string;
  event_type: "process_start" | "process_stop" | "system";
}

export interface HzPoint {
  time: number;
  hz: number;
}
