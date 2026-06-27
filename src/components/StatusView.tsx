import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  HzChangedPayload,
  LogEntry,
  HzPoint,
  MonitorInfoExtended,
  WatchedProcess,
} from "../types";
import { wpName, wpKey } from "../types";

interface Props {
  monitorName: string;
  watchedProcesses: WatchedProcess[];
  gameHz?: number;
}

let logIdCounter = 0;
const ICON_CACHE_KEY = "hz-process-icons";

function loadIconCache(): Record<string, string | undefined> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(ICON_CACHE_KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    return Object.fromEntries(
      Object.entries(parsed).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
    );
  } catch {
    return {};
  }
}

function saveIconToCache(name: string, icon: string) {
  const cache = loadIconCache();
  cache[name] = icon;
  try {
    localStorage.setItem(ICON_CACHE_KEY, JSON.stringify(cache));
  } catch {
    // Cache writes are best-effort; private mode or quota limits can fail here.
  }
}

export function StatusView({ monitorName, watchedProcesses, gameHz }: Props) {
  const [currentHz, setCurrentHz] = useState<number | null>(null);
  const [mode, setMode] = useState<"STANDARD" | "GAME">("STANDARD");
  const [monitorLabel, setMonitorLabel] = useState<string>("");
  const [runningProcesses, setRunningProcesses] = useState<string[]>([]);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [logFilter, setLogFilter] = useState<"all" | "hz" | "process">("all");
  const [hzHistory, setHzHistory] = useState<HzPoint[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [todaySwitches, setTodaySwitches] = useState(0);
  const [processIcons, setProcessIcons] = useState<Record<string, string | null | undefined>>(
    () => loadIconCache()
  );
  const hzRef = useRef<HTMLSpanElement>(null);

  function addLog(payload: HzChangedPayload) {
    const timestamp = new Date().toLocaleTimeString("de-DE", {
      hour: "2-digit",
      minute: "2-digit",
    });
    setLog((prev) =>
      [
        {
          id: ++logIdCounter,
          timestamp,
          message: payload.reason,
          hz_from: payload.hz_from,
          hz_to: payload.hz_to,
          process_name: payload.process_name,
          event_type: payload.event_type ?? "system",
        } as LogEntry,
        ...prev,
      ].slice(0, 50)
    );
    if (payload.event_type !== "system") {
      setTodaySwitches((n) => n + 1);
    }
  }

  const refreshStatus = useCallback(() => {
    if (monitorName) {
      invoke<number>("get_current_hz", { monitorName })
        .then((hz) => {
          const timestamp = Date.now();
          setNow(timestamp);
          setCurrentHz(hz);
          setHzHistory((prev) =>
            prev.length === 0 ? [{ time: timestamp, hz }] : prev
          );
        })
        .catch(() => undefined);
    }
    invoke<string[]>("get_running_watched")
      .then(setRunningProcesses)
      .catch(() => undefined);
  }, [monitorName]);

  useEffect(() => {
    refreshStatus();

    const unlisten = listen<HzChangedPayload>("hz-changed", (event) => {
      const hz = event.payload.current_hz;
      const timestamp = Date.now();
      setNow(timestamp);
      setCurrentHz(hz);
      const isGame = event.payload.event_type === "process_start";
      setMode(isGame ? "GAME" : "STANDARD");
      setHzHistory((prev) => {
        const filtered = prev.filter((p) => p.time >= timestamp - 3_600_000);
        return [...filtered, { time: timestamp, hz }];
      });
      addLog(event.payload);
      invoke<string[]>("get_running_watched").then(setRunningProcesses).catch(() => undefined);
    });

    const interval = setInterval(refreshStatus, 5000);

    return () => {
      void unlisten.then((fn) => fn());
      clearInterval(interval);
    };
  }, [monitorName, refreshStatus]);

  useEffect(() => {
    if (!monitorName) return;
    invoke<MonitorInfoExtended[]>("get_monitors_extended")
      .then((mons) => {
        const mon = mons.find((m) => m.device_name === monitorName);
        setMonitorLabel(mon?.friendly_name ?? monitorName);
      })
      .catch(() => setMonitorLabel(monitorName));
  }, [monitorName]);

  useEffect(() => {
    const cache = loadIconCache();
    for (const wp of watchedProcesses) {
      const name = wpName(wp);
      const iconKey = name.toLowerCase();
      if (processIcons[iconKey] || cache[iconKey]) continue;
      invoke<string | null>("get_process_icon", { processName: name, exePath: typeof wp === "object" ? wp.path : undefined })
        .then((icon) => {
          if (icon) {
            saveIconToCache(iconKey, icon);
            setProcessIcons((prev) => ({ ...prev, [iconKey]: icon }));
          }
        })
        .catch(() => {});
    }
  }, [watchedProcesses, processIcons]);

  const { gameMinutes, standardMinutes } = useMemo(() => {
    const threshold = gameHz ?? 100;
    const gameMs = hzHistory.reduce((acc, pt, i) => {
      if (i === 0) return acc;
      const prev = hzHistory[i - 1];
      const dur = pt.time - prev.time;
      return acc + (prev.hz >= threshold ? dur : 0);
    }, 0);
    const totalMs = hzHistory.length > 1
      ? hzHistory[hzHistory.length - 1].time - hzHistory[0].time
      : 0;
    return {
      gameMinutes: Math.round(gameMs / 60000),
      standardMinutes: Math.round((totalMs - gameMs) / 60000),
    };
  }, [gameHz, hzHistory]);

  const filteredLog = log.filter((e) => {
    if (logFilter === "all") return true;
    if (logFilter === "hz") return e.hz_from != null && e.hz_to != null;
    return e.process_name != null;
  });

  useEffect(() => {
    const el = hzRef.current;
    if (!el || currentHz == null) return;
    el.classList.remove("hz-pop");
    void el.offsetWidth;
    el.classList.add("hz-pop");
  }, [currentHz]);

  function formatDuration(minutes: number) {
    const h = Math.floor(minutes / 60);
    const m = minutes % 60;
    if (h > 0) return `${h}h ${m}m`;
    return `${m}m`;
  }

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-4">
        {/* Current Hz card */}
        <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-1">
          <div className="flex items-start justify-between mb-2">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Aktuelle Bildwiederholrate</span>
            <span
              key={mode}
              className={`text-xs font-bold px-2.5 py-0.5 rounded-full badge-anim ${
                mode === "GAME"
                  ? "bg-red-100 dark:bg-red-900/40 text-red-600 dark:text-red-400"
                  : "bg-slate-200 dark:bg-slate-700 text-slate-600 dark:text-slate-300"
              }`}
            >
              {mode === "GAME" ? "● GAME" : "● STANDARD"}
            </span>
          </div>
          <div className="flex items-baseline gap-1.5">
            <span
              ref={hzRef}
              className="text-6xl font-black text-slate-900 dark:text-slate-100 leading-none tabular-nums"
            >
              {currentHz ?? "—"}
            </span>
            {currentHz != null && (
              <span className="text-2xl font-semibold text-slate-400 dark:text-slate-500">Hz</span>
            )}
          </div>
          {monitorLabel && (
            <p className="text-xs text-slate-400 dark:text-slate-500 mt-1 font-mono truncate">
              {monitorLabel}
            </p>
          )}
          <Sparkline points={hzHistory} mode={mode} now={now} />
          <div className="flex justify-between text-xs text-slate-400 dark:text-slate-500 mt-1">
            <span>Letzte Stunde</span>
            <span>{gameMinutes} Min Game · {standardMinutes} Min Standard</span>
          </div>
        </div>

        {/* Active Processes card */}
        <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-2">
          <div className="flex items-center justify-between mb-3">
            <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Aktive Prozesse</span>
            <span className="text-xs text-slate-400 dark:text-slate-500">
              {runningProcesses.length} von {watchedProcesses.length} läuft
            </span>
          </div>
          {watchedProcesses.length === 0 ? (
            <p className="text-xs text-slate-400 dark:text-slate-500 italic">Keine Prozesse konfiguriert.</p>
          ) : (
            <div className="space-y-2">
              {watchedProcesses.slice(0, 5).map((wp) => {
                const key = wpKey(wp);
                const name = wpName(wp);
                const iconKey = name.toLowerCase();
                const icon = processIcons[iconKey] ?? loadIconCache()[iconKey] ?? null;
                const isRunning = runningProcesses.some((r) => r === key);
                return (
                  <div key={key} className="flex items-center gap-2.5">
                    <div
                      className={`w-7 h-7 rounded-lg flex items-center justify-center shrink-0 overflow-hidden ${
                        icon
                          ? "bg-transparent"
                          : isRunning
                            ? "bg-red-500 shadow-sm shadow-red-500/30"
                            : "bg-slate-200 dark:bg-slate-700"
                      }`}
                    >
                      {icon ? (
                        <img src={icon} alt="" className="w-7 h-7 object-contain" />
                      ) : (
                        <svg width="8" height="8" viewBox="0 0 8 8" fill="none">
                          <path d="M2.5 1.5l4 2.5-4 2.5V1.5z" fill={isRunning ? "white" : "#94a3b8"} />
                        </svg>
                      )}
                    </div>
                    <span
                      className={`text-sm font-mono flex-1 truncate ${
                        isRunning
                          ? "text-slate-900 dark:text-slate-100 font-semibold"
                          : "text-slate-500 dark:text-slate-400"
                      }`}
                    >
                      {name}
                    </span>
                    <span
                      className={`text-xs shrink-0 font-medium ${
                        isRunning ? "text-slate-600 dark:text-slate-300" : "text-slate-400 dark:text-slate-500"
                      }`}
                    >
                      {isRunning ? "Läuft" : "Wartet"}
                    </span>
                  </div>
                );
              })}
              {watchedProcesses.length > 5 && (
                <p className="text-xs text-slate-400 dark:text-slate-500 pl-9">
                  +{watchedProcesses.length - 5} weitere
                </p>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Today stats */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] px-5 py-4 flex items-center gap-8 anim-fade-up stagger-3">
        <div>
          <div className="text-2xl font-black text-slate-900 dark:text-slate-100">{todaySwitches}</div>
          <div className="text-xs text-slate-400 dark:text-slate-500">× gewechselt<br/>automatisch</div>
        </div>
        <div>
          <div className="text-2xl font-black text-red-500">{formatDuration(gameMinutes)}</div>
          <div className="text-xs text-slate-400 dark:text-slate-500">im Game-Mode</div>
        </div>
        <div>
          <div className="text-2xl font-black text-slate-900 dark:text-slate-100">{formatDuration(standardMinutes)}</div>
          <div className="text-xs text-slate-400 dark:text-slate-500">im Standard</div>
        </div>
        <div className="ml-auto text-xs text-slate-400 dark:text-slate-500">Heute</div>
      </div>

      {/* Event log */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-4">
        <div className="flex items-center justify-between mb-4">
          <span className="text-sm font-semibold text-slate-800 dark:text-slate-100">Ereignisprotokoll</span>
          <div className="flex gap-1">
            {(
              [
                { id: "all", label: "Alle" },
                { id: "hz", label: "Hz-Wechsel" },
                { id: "process", label: "Prozesse" },
              ] as const
            ).map(({ id, label }) => (
              <button
                key={id}
                onClick={() => setLogFilter(id)}
                className={`px-3 py-1 text-xs rounded-lg font-medium transition-colors ${
                  logFilter === id
                    ? "bg-red-500 text-white"
                    : "text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200 hover:bg-black/5 dark:hover:bg-white/5"
                }`}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
        <div className="space-y-3 min-h-20">
          {filteredLog.length === 0 ? (
            <p className="text-sm text-slate-400 dark:text-slate-500 italic">Noch keine Ereignisse.</p>
          ) : (
            filteredLog.map((entry, i) => (
              <div
                key={entry.id}
                className="flex items-center gap-3 text-sm log-entry"
                style={{ animationDelay: `${Math.min(i, 4) * 30}ms` }}
              >
                <span className="text-slate-400 dark:text-slate-500 text-xs font-mono w-10 shrink-0 tabular-nums">
                  {entry.timestamp}
                </span>
                <span
                  className={`w-2 h-2 rounded-full shrink-0 ${
                    entry.event_type === "process_start"
                      ? "bg-red-500 dot-pulse"
                      : "bg-slate-400 dark:bg-slate-500"
                  }`}
                />
                <span className="flex-1 text-slate-700 dark:text-slate-300 truncate">{entry.message}</span>
                {entry.hz_from != null && entry.hz_to != null && (
                  <span className="text-xs text-slate-500 dark:text-slate-400 shrink-0 tabular-nums font-mono">
                    {entry.hz_from} → {entry.hz_to} Hz
                  </span>
                )}
                {entry.event_type !== "system" && (
                  <span
                    className={`text-xs font-bold px-2.5 py-0.5 rounded-full shrink-0 ${
                      entry.event_type === "process_start"
                        ? "bg-red-100 dark:bg-red-900/40 text-red-600 dark:text-red-400"
                        : "bg-slate-200 dark:bg-slate-700 text-slate-600 dark:text-slate-300"
                    }`}
                  >
                    {entry.event_type === "process_stop" ? "STANDBY" : "GAME"}
                  </span>
                )}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

function Sparkline({ points, mode, now }: { points: HzPoint[]; mode: string; now: number }) {
  if (points.length < 2) return <div style={{ height: 64 }} className="mt-3" />;

  const W = 400;
  const H = 52;
  const oneHourAgo = now - 3_600_000;

  const inWindow = points.filter((p) => p.time >= oneHourAgo);
  if (inWindow.length === 0) return <div style={{ height: 64 }} className="mt-3" />;

  const display: HzPoint[] = [];
  if (inWindow[0].time > oneHourAgo) {
    display.push({ time: oneHourAgo, hz: inWindow[0].hz });
  }
  display.push(...inWindow);
  display.push({ time: now, hz: display[display.length - 1].hz });

  const minHz = Math.min(...display.map((p) => p.hz));
  const maxHz = Math.max(...display.map((p) => p.hz));
  const hzRange = maxHz - minHz || 1;
  const PAD = 4;

  const toX = (t: number) => ((t - oneHourAgo) / (now - oneHourAgo)) * W;
  const toY = (hz: number) => H - PAD - ((hz - minHz) / hzRange) * (H - PAD * 2);

  let d = `M ${toX(display[0].time).toFixed(1)} ${toY(display[0].hz).toFixed(1)}`;
  for (let i = 1; i < display.length; i++) {
    const x = toX(display[i].time).toFixed(1);
    const prevY = toY(display[i - 1].hz).toFixed(1);
    const y = toY(display[i].hz).toFixed(1);
    d += ` L ${x} ${prevY} L ${x} ${y}`;
  }

  const areaD = `${d} L ${W} ${H} L 0 ${H} Z`;
  const color = mode === "GAME" ? "#ef4444" : "#94a3b8";

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      className="mt-3 w-full"
      style={{ height: 64, display: "block" }}
    >
      <defs>
        <linearGradient id="hzGradFill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.3" />
          <stop offset="100%" stopColor={color} stopOpacity="0.02" />
        </linearGradient>
      </defs>
      <path d={areaD} fill="url(#hzGradFill)" />
      <path d={d} fill="none" stroke={color} strokeWidth="2" />
    </svg>
  );
}
