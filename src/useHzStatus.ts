import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import type { HzChangedPayload } from "./types";

/** Latest Hz + running watched keys, shared by the header and the status view. */
export interface HzStatus {
  currentHz: number | null;
  running: string[];
  /** Increments on every hz-changed event so consumers can react to one. */
  lastEvent: HzChangedPayload | null;
}

type Listener = (s: HzStatus) => void;

// ponytail: one module-level store instead of a context provider — there is
// exactly one backend to poll, and two components that need it. A provider adds
// a wrapper and no behaviour.
let state: HzStatus = { currentHz: null, running: [], lastEvent: null };
const listeners = new Set<Listener>();
let monitorName = "";
let timer: ReturnType<typeof setInterval> | null = null;
let unlisten: (() => void) | null = null;
// listen() resolves asynchronously, so a stop/start round-trip can leave a
// registration in flight. The generation is bumped on every stop: a handle that
// resolves for a superseded generation is dropped on arrival instead of
// overwriting the live one (which would leak the newer listener forever).
let generation = 0;

function emit(next: Partial<HzStatus>) {
  state = { ...state, ...next };
  for (const l of listeners) l(state);
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

function start() {
  const gen = generation;
  // Poll so manual Hz changes made in Windows — which fire no event — still show.
  timer = setInterval(refresh, 5000);
  void listen<HzChangedPayload>("hz-changed", (e) => {
    // A listener from a superseded generation may still fire between resolving
    // and being torn down below; its updates are not wanted.
    if (gen !== generation) return;
    emit({ currentHz: e.payload.current_hz, lastEvent: e.payload });
    // The event says what we set; the running set says which mode we're in.
    invoke<string[]>("get_running_watched")
      .then((running) => emit({ running }))
      .catch(() => undefined);
  }).then((fn) => {
    // A teardown that ran before the listener resolved must still take effect,
    // and so must one that already started a newer generation.
    if (gen !== generation || listeners.size === 0) fn();
    else unlisten = fn;
  });
  refresh();
}

function stop() {
  // Invalidate any registration still in flight from this generation.
  generation++;
  if (timer) clearInterval(timer);
  timer = null;
  unlisten?.();
  unlisten = null;
}

/**
 * Subscribes to the shared Hz status. Pass the configured monitor; the first
 * subscriber starts the single poller, the last one stops it.
 */
export function useHzStatus(monitor?: string): HzStatus {
  const [snapshot, setSnapshot] = useState(state);

  useEffect(() => {
    if (monitor !== undefined && monitor !== monitorName) {
      monitorName = monitor;
      // Monitor changed — the cached Hz belongs to the old one.
      emit({ currentHz: null });
      if (listeners.size > 0) refresh();
    }
  }, [monitor]);

  useEffect(() => {
    const listener: Listener = (s) => setSnapshot(s);
    listeners.add(listener);
    if (listeners.size === 1) start();
    else setSnapshot(state);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) stop();
    };
  }, []);

  return snapshot;
}
