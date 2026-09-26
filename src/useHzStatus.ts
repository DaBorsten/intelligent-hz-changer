import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useSyncExternalStore } from "react";
import type { HzChangedPayload, HzErrorPayload } from "./types";
import { visibleInterval } from "./visibleInterval";

/** Latest Hz + running watched keys, shared by the header and the status view. */
export interface HzStatus {
  currentHz: number | null;
  running: string[];
  /** The most recent switch the display driver refused, until it is either
   * dismissed or superseded by a switch that worked. */
  lastError: HzErrorPayload | null;
}

type Listener = () => void;
type EventListener = (event: HzChangedPayload) => void;

// ponytail: one module-level store instead of a context provider — there is
// exactly one backend to poll, and two components that need it. A provider adds
// a wrapper and no behaviour.
let state: HzStatus = {
  currentHz: null,
  running: [],
  lastError: null,
};
const listeners = new Set<Listener>();
const eventListeners = new Set<EventListener>();
let monitorName = "";
let stopPolling: (() => void) | null = null;
const unlisteners: (() => void)[] = [];
// listen() resolves asynchronously, so a stop/start round-trip can leave a
// registration in flight. The generation is bumped on every stop: a handle that
// resolves for a superseded generation is dropped on arrival instead of
// overwriting the live one (which would leak the newer listener forever).
let generation = 0;

function emit(next: Partial<HzStatus>) {
  state = { ...state, ...next };
  for (const l of listeners) l();
}

function refresh() {
  if (monitorName) {
    invoke<number>("get_current_hz", { monitorName })
      .then((hz) => emit({ currentHz: hz }))
      .catch(() => undefined);
  }
  invoke<string[]>("get_running_watched")
    .then((running) => emit({ running }))
    .catch(() => undefined);
}

/** Keeps a registration only while it is still current — a teardown that ran
 * before `listen` resolved must still take effect, and so must one that already
 * started a newer generation. */
function keep(gen: number, fn: () => void) {
  if (gen !== generation || listeners.size === 0) fn();
  else unlisteners.push(fn);
}

function start() {
  const gen = generation;
  // Poll so manual Hz changes made in Windows — which fire no event — still show.
  stopPolling = visibleInterval(refresh, 5000);
  void listen<HzChangedPayload>("hz-changed", (e) => {
    // A listener from a superseded generation may still fire between resolving
    // and being torn down below; its updates are not wanted.
    if (gen !== generation) return;
    // A switch that worked answers whatever the last failed one reported. Only
    // the reported monitor's switches say anything about `currentHz`.
    const ours = !e.payload.monitor || e.payload.monitor === monitorName;
    emit(
      ours
        ? { currentHz: e.payload.current_hz, lastError: null }
        : { lastError: null },
    );
    for (const l of eventListeners) l(e.payload);
    // The event says what we set; the running set says which mode we're in.
    invoke<string[]>("get_running_watched")
      .then((running) => emit({ running }))
      .catch(() => undefined);
  }).then((fn) => keep(gen, fn));

  void listen<HzErrorPayload>("hz-error", (e) => {
    if (gen !== generation) return;
    emit({ lastError: e.payload });
  }).then((fn) => keep(gen, fn));

  refresh();
}

function stop() {
  // Invalidate any registration still in flight from this generation.
  generation++;
  stopPolling?.();
  stopPolling = null;
  for (const fn of unlisteners.splice(0)) fn();
}

/** Dismisses the current Hz error banner. */
export function clearHzError() {
  emit({ lastError: null });
}

function subscribe(listener: Listener) {
  listeners.add(listener);
  if (listeners.size === 1) start();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) stop();
  };
}

function getSnapshot() {
  return state;
}

/**
 * Calls `listener` once per hz-changed event that arrives while the poller runs.
 * Events that arrived before subscribing are not replayed.
 */
export function onHzChanged(listener: EventListener) {
  eventListeners.add(listener);
  return () => {
    eventListeners.delete(listener);
  };
}

/**
 * Subscribes to the shared Hz status. Pass the configured monitor; the first
 * subscriber starts the single poller, the last one stops it.
 */
export function useHzStatus(monitor?: string): HzStatus {
  useEffect(() => {
    if (monitor !== undefined && monitor !== monitorName) {
      monitorName = monitor;
      // Monitor changed — the cached Hz belongs to the old one.
      emit({ currentHz: null });
      if (listeners.size > 0) refresh();
    }
  }, [monitor]);

  return useSyncExternalStore(subscribe, getSnapshot);
}
