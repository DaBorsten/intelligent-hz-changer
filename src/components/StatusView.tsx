import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { SegmentedControl } from "./SegmentedControl";
import type {
  HzChangedPayload,
  HzPoint,
  LogEntry,
  MonitorInfoExtended,
  WatchedProcess,
} from "../types";
import { wpKey, wpName } from "../types";
import { deviceTag } from "../monitors";
import { onHzChanged, useHzStatus } from "../useHzStatus";

interface Props {
  monitorName: string;
  watchedProcesses: WatchedProcess[];
  gameHz?: number;
  active: boolean;
}

const ICON_CACHE_KEY = "hz-process-icons";

function loadIconCache(): Record<string, string | undefined> {
  try {
    const parsed: unknown = JSON.parse(
      localStorage.getItem(ICON_CACHE_KEY) ?? "{}",
    );
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      ),
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

export function StatusView({
  monitorName,
  watchedProcesses,
  gameHz,
  active,
}: Props) {
  const { t, i18n } = useTranslation();
  const { currentHz, running: runningProcesses } = useHzStatus(monitorName);
  const [monitorLabel, setMonitorLabel] = useState<string>("");
  const [log, setLog] = useState<LogEntry[]>([]);
  const [logFilter, setLogFilter] = useState<"all" | "hz" | "process">("all");
  const [hzHistory, setHzHistory] = useState<HzPoint[]>(() =>
    currentHz == null ? [] : [{ time: Date.now(), hz: currentHz }],
  );
  const [now, setNow] = useState(() => Date.now());
  const [processIcons, setProcessIcons] = useState<
    Record<string, string | null | undefined>
  >(() => loadIconCache());
  const hzRef = useRef<HTMLSpanElement>(null);

  // ponytail: mode is derived from the running set (source of truth), not the
  // hz-changed event — a game already at target Hz on startup fires no event.
  // Same expression the header pill uses, off the same shared store.
  const mode: "STANDARD" | "GAME" =
    // ponytail: disabled means we change nothing, so the badge stays STANDARD.
    active && runningProcesses.length > 0 ? "GAME" : "STANDARD";

  // The backend keeps the recent events, since this window is destroyed while
  // the app sits in the tray. Seed from it on mount, then append live events;
  // `id` is unique per event, so one that arrives both ways is kept once.
  useEffect(() => {
    const toEntry = (event: HzChangedPayload): LogEntry => ({
      id: event.id,
      time: event.time,
      timestamp: new Date(event.time).toLocaleTimeString(i18n.language, {
        hour: "2-digit",
        minute: "2-digit",
      }),
      message: event.reason,
      hz_from: event.hz_from,
      hz_to: event.hz_to,
      process_name: event.process_name,
      event_type: event.event_type ?? "system",
      monitor: event.monitor,
      batch: event.batch,
    });
    const merge = (incoming: LogEntry[]) =>
      setLog((prev) => {
        const seen = new Set(prev.map((e) => e.id));
        return [...prev, ...incoming.filter((e) => !seen.has(e.id))]
          .sort((a, b) => b.id - a.id)
          .slice(0, 50);
      });

    let cancelled = false;
    invoke<HzChangedPayload[]>("get_hz_log")
      .then((events) => {
        if (cancelled) return;
        merge(events.map(toEntry));
        const cutoff = Date.now() - 3_600_000;
        const points = events
          .filter(
            (e) =>
              e.time >= cutoff &&
              e.hz_to != null &&
              (!e.monitor || e.monitor === monitorName),
          )
          .map((e) => ({ time: e.time, hz: e.hz_to as number }));
        setHzHistory((prev) => {
          const seen = new Set(prev.map((p) => p.time));
          return [...points.filter((p) => !seen.has(p.time)), ...prev].sort(
            (a, b) => a.time - b.time,
          );
        });
      })
      .catch(() => undefined);
    const off = onHzChanged((event) => merge([toEntry(event)]));
    return () => {
      cancelled = true;
      off();
    };
  }, [i18n, monitorName]);

  // Derived from the ticking `now` so the count resets at midnight; the string
  // only changes once a day, so the memo below doesn't rerun every tick.
  const today = new Date(now).toDateString();
  // Only real process edges count as an automatic switch; "system" covers
  // the startup ping, pause/resume and config saves.
  const todaySwitches = useMemo(() => {
    // One game start that switched several monitors is still one switch.
    return new Set(
      log
        .filter(
          (e) =>
            e.event_type !== "system" &&
            new Date(e.time).toDateString() === today,
        )
        .map((e) => e.batch ?? -e.id),
    ).size;
  }, [log, today]);

  // Name the monitor per entry only once there is more than one to tell apart.
  const multiMonitorLog =
    new Set(log.map((e) => e.monitor).filter(Boolean)).size > 1;

  // Track Hz over time for the sparkline, trimmed to the displayed hour.
  // Adjusted during render rather than in an effect, so the new sample lands in
  // the same render as the new rate.
  const [trackedHz, setTrackedHz] = useState(currentHz);
  if (currentHz !== trackedHz) {
    setTrackedHz(currentHz);
    if (currentHz != null) {
      setHzHistory((prev) => {
        const timestamp = Date.now();
        const filtered = prev.filter((p) => p.time >= timestamp - 3_600_000);
        const last = filtered[filtered.length - 1];
        // Returning to the same rate after a monitor switch (null in between)
        // needs no new point; the line already extends to "now".
        if (last && last.hz === currentHz) return filtered;
        return [...filtered, { time: timestamp, hz: currentHz }];
      });
    }
  }

  // A sample can be newer than the last tick; the right edge must not lag it.
  const sparklineNow = Math.max(
    now,
    hzHistory[hzHistory.length - 1]?.time ?? 0,
  );

  // Keep the sparkline's right edge moving even while Hz holds steady.
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    if (!monitorName) return;
    invoke<MonitorInfoExtended[]>("get_monitors_extended")
      .then((mons) => {
        const mon = mons.find((m) => m.device_name === monitorName);
        setMonitorLabel(mon?.friendly_name ?? monitorName);
      })
      .catch(() => setMonitorLabel(monitorName));
  }, [monitorName]);

  // Icons already requested this session, so a re-render never re-invokes for
  // the same process. A ref rather than state: this must not itself re-trigger
  // the effect that writes it.
  const requestedIcons = useRef<Set<string>>(new Set());

  useEffect(() => {
    for (const wp of watchedProcesses) {
      const name = wpName(wp);
      const iconKey = name.toLowerCase();
      // processIcons is seeded from the localStorage cache at mount, so a
      // cached icon must short-circuit here too — requestedIcons is empty on
      // every mount and would otherwise re-invoke on each tab switch.
      if (requestedIcons.current.has(iconKey) || processIcons[iconKey])
        continue;
      requestedIcons.current.add(iconKey);
      invoke<string | null>("get_process_icon", {
        processName: name,
        exePath: typeof wp === "object" ? wp.path : undefined,
      })
        .then((icon) => {
          if (icon) {
            saveIconToCache(iconKey, icon);
            setProcessIcons((prev) => ({ ...prev, [iconKey]: icon }));
          } else {
            // No icon yet — the process isn't running, so its exe path is
            // unresolvable. Unmark so a later attempt can succeed.
            requestedIcons.current.delete(iconKey);
          }
        })
        .catch(() => {
          // Allow a retry on the next config change.
          requestedIcons.current.delete(iconKey);
        });
    }
    // processIcons re-runs this on each resolved icon, but every already-known
    // key short-circuits above, so it settles instead of looping.
  }, [watchedProcesses, processIcons]);

  const { gameMinutes, standardMinutes } = useMemo(() => {
    const threshold = gameHz ?? 100;
    const gameMs = hzHistory.reduce((acc, pt, i) => {
      if (i === 0) return acc;
      const prev = hzHistory[i - 1];
      const dur = pt.time - prev.time;
      return acc + (prev.hz >= threshold ? dur : 0);
    }, 0);
    const totalMs =
      hzHistory.length > 1
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
      <div className="grid grid-cols-[repeat(auto-fit,minmax(320px,1fr))] gap-4">
        {/* Current Hz card */}
        <div className="card p-5 flex flex-col gap-1.5 min-h-62 anim-fade-up stagger-1">
          <div className="flex items-center justify-between">
            <span className="font-semibold">{t("status.currentHz")}</span>
            <span
              key={mode}
              className={`flex items-center gap-1.5 text-[11px] font-medium px-2.25 py-0.5 rounded-full border badge-anim ${
                mode === "GAME"
                  ? "bg-accent-soft border-accent-line text-accent-text"
                  : "bg-chip border-line-strong text-fg-3"
              }`}
            >
              <span
                className={`w-1.5 h-1.5 rounded-full ${mode === "GAME" ? "bg-accent" : "bg-faint"}`}
              />
              {t(mode === "GAME" ? "mode.game" : "mode.standard")}
            </span>
          </div>
          <div className="flex items-baseline gap-2 mt-3.5">
            <span
              ref={hzRef}
              className="text-[64px] font-semibold leading-none tracking-[-0.03em] tabular-nums"
            >
              {currentHz ?? "—"}
            </span>
            {currentHz != null && (
              <span className="text-xl font-medium text-faint">Hz</span>
            )}
          </div>
          {monitorLabel && (
            <p className="text-[11px] text-muted font-mono truncate">
              {monitorLabel}
            </p>
          )}
          <div className="flex-1" />
          <Sparkline points={hzHistory} mode={mode} now={sparklineNow} />
          <div className="flex justify-between border-t border-line-subtle pt-3 mt-1.5 text-xs text-muted">
            <span>{t("status.lastHour")}</span>
            <span>
              {t("status.timeGame", {
                game: gameMinutes,
                standard: standardMinutes,
              })}
            </span>
          </div>
        </div>

        {/* Active Processes card */}
        <div className="card overflow-hidden anim-fade-up stagger-2">
          <div className="flex items-center justify-between px-4 pt-4 pb-2.5">
            <span className="font-semibold">{t("status.activeProcesses")}</span>
            <span className="text-xs text-muted">
              {t("status.runningOf", {
                running: runningProcesses.length,
                total: watchedProcesses.length,
              })}
            </span>
          </div>
          {watchedProcesses.length === 0 ? (
            <p className="text-xs text-muted italic px-4 pb-4">
              {t("status.noProcesses")}
            </p>
          ) : (
            <>
              {watchedProcesses.slice(0, 5).map((wp) => {
                const key = wpKey(wp);
                const name = wpName(wp);
                const iconKey = name.toLowerCase();
                // processIcons is seeded from the cache at mount, so no
                // localStorage read is needed per row per render.
                const icon = processIcons[iconKey] ?? null;
                const isRunning = runningProcesses.some((r) => r === key);
                return (
                  <div
                    key={key}
                    className="flex items-center gap-3 px-4 py-2 border-t border-line-subtle"
                  >
                    <div
                      className={`w-6.5 h-6.5 rounded-[7px] flex items-center justify-center shrink-0 overflow-hidden text-xs font-semibold ${
                        icon
                          ? "bg-transparent"
                          : isRunning
                            ? "bg-accent text-white"
                            : "bg-chip border border-line-strong text-fg-2"
                      }`}
                    >
                      {icon ? (
                        <img
                          src={icon}
                          alt=""
                          className="w-6.5 h-6.5 object-contain"
                        />
                      ) : (
                        name.charAt(0).toUpperCase()
                      )}
                    </div>
                    <span className="text-[12.5px] font-mono flex-1 min-w-0 truncate">
                      {name}
                    </span>
                    <span
                      className={`flex items-center gap-1.5 text-xs shrink-0 ${
                        isRunning ? "text-accent-text" : "text-muted"
                      }`}
                    >
                      <span
                        className={`w-1.5 h-1.5 rounded-full ${
                          isRunning ? "bg-accent" : "border border-faint"
                        }`}
                      />
                      {isRunning ? t("status.running") : t("status.waiting")}
                    </span>
                  </div>
                );
              })}
              {watchedProcesses.length > 5 && (
                <p className="text-xs text-muted px-4 py-2 border-t border-line-subtle">
                  {t("status.moreProcesses", {
                    count: watchedProcesses.length - 5,
                  })}
                </p>
              )}
            </>
          )}
        </div>
      </div>

      {/* Today stats */}
      <div className="card px-5 py-4 flex items-center flex-wrap gap-10 anim-fade-up stagger-3">
        <div>
          <div className="text-[22px] font-semibold tabular-nums">
            {todaySwitches}
          </div>
          <div className="text-xs text-muted mt-0.5 whitespace-pre-line">
            {t("status.switchedAuto")}
          </div>
        </div>
        <div>
          <div className="text-[22px] font-semibold tabular-nums text-accent-text">
            {formatDuration(gameMinutes)}
          </div>
          <div className="text-xs text-muted mt-0.5">
            {t("status.inGameMode")}
          </div>
        </div>
        <div>
          <div className="text-[22px] font-semibold tabular-nums">
            {formatDuration(standardMinutes)}
          </div>
          <div className="text-xs text-muted mt-0.5">
            {t("status.inStandard")}
          </div>
        </div>
        <div className="ml-auto text-xs text-muted">{t("status.today")}</div>
      </div>

      {/* Event log */}
      <div className="card overflow-hidden anim-fade-up stagger-4">
        <div className="flex items-center justify-between flex-wrap gap-3 px-4 py-3">
          <span className="font-semibold">{t("status.eventLog")}</span>
          <SegmentedControl
            options={[
              { value: "all", label: t("status.filterAll") },
              { value: "hz", label: t("status.filterHz") },
              { value: "process", label: t("status.filterProcess") },
            ]}
            value={logFilter}
            onChange={(v) => setLogFilter(v as typeof logFilter)}
          />
        </div>
        {filteredLog.length === 0 ? (
          <p
            key={logFilter}
            className="text-muted italic px-4 py-3 border-t border-line-subtle log-entry"
          >
            {t("status.noEvents")}
          </p>
        ) : (
          filteredLog.map((entry, i) => {
            // Backend reasons lead with the process name; that part reads as code.
            const verb =
              entry.process_name &&
              entry.message.startsWith(entry.process_name)
                ? entry.message.slice(entry.process_name.length)
                : null;
            return (
              <div
                key={entry.id}
                className="flex items-center gap-3.5 px-4 py-2.5 border-t border-line-subtle log-entry"
                style={{ animationDelay: `${Math.min(i, 4) * 30}ms` }}
              >
                <span className="text-muted text-xs font-mono w-10 shrink-0 tabular-nums">
                  {entry.timestamp}
                </span>
                <span
                  className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                    entry.event_type === "process_start"
                      ? "bg-accent"
                      : "bg-dot"
                  }`}
                />
                <span className="flex-1 min-w-0 truncate">
                  {verb === null ? (
                    entry.message
                  ) : (
                    <>
                      <span className="font-mono text-[12.5px]">
                        {entry.process_name}
                      </span>
                      <span className="text-muted">{verb}</span>
                    </>
                  )}
                </span>
                {multiMonitorLog && entry.monitor && (
                  <span
                    className="text-[10px] font-semibold px-1.5 py-0.5 rounded-md shrink-0 bg-chip border border-line-strong text-fg-3"
                    title={entry.monitor}
                  >
                    {t("status.monitorTag", { n: deviceTag(entry.monitor) })}
                  </span>
                )}
                {entry.hz_from != null && entry.hz_to != null && (
                  <span className="text-xs text-fg-3 shrink-0 tabular-nums font-mono">
                    {entry.hz_from} → {entry.hz_to} Hz
                  </span>
                )}
                {entry.event_type !== "system" ? (
                  <span
                    className={`w-18 text-center text-[11px] font-semibold py-0.5 rounded-full border shrink-0 ${
                      entry.event_type === "process_start"
                        ? "bg-accent-soft border-accent-line text-accent-text"
                        : "bg-chip border-line-strong text-fg-3"
                    }`}
                  >
                    {entry.event_type === "process_stop"
                      ? t("status.badgeStandby")
                      : t("status.badgeGame")}
                  </span>
                ) : (
                  <span className="w-18 shrink-0" />
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function Sparkline({
  points,
  mode,
  now,
}: {
  points: HzPoint[];
  mode: string;
  now: number;
}) {
  if (points.length < 2) return <div style={{ height: 64 }} className="mt-3" />;

  const W = 400;
  const H = 52;
  const oneHourAgo = now - 3_600_000;

  const inWindow = points.filter((p) => p.time >= oneHourAgo);
  if (inWindow.length === 0)
    return <div style={{ height: 64 }} className="mt-3" />;

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
  const toY = (hz: number) =>
    H - PAD - ((hz - minHz) / hzRange) * (H - PAD * 2);

  let d = `M ${toX(display[0].time).toFixed(1)} ${toY(display[0].hz).toFixed(1)}`;
  for (let i = 1; i < display.length; i++) {
    const x = toX(display[i].time).toFixed(1);
    const prevY = toY(display[i - 1].hz).toFixed(1);
    const y = toY(display[i].hz).toFixed(1);
    d += ` L ${x} ${prevY} L ${x} ${y}`;
  }

  const areaD = `${d} L ${W} ${H} L 0 ${H} Z`;
  const color = mode === "GAME" ? "var(--accent)" : "var(--faint)";

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      className="mt-3 w-full"
      style={{ height: 64, display: "block" }}
    >
      <defs>
        <linearGradient id="hzGradFill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" style={{ stopColor: color, stopOpacity: 0.25 }} />
          <stop offset="100%" style={{ stopColor: color, stopOpacity: 0.02 }} />
        </linearGradient>
      </defs>
      <path d={areaD} fill="url(#hzGradFill)" />
      <path
        d={d}
        fill="none"
        className="sparkline-path"
        style={{ stroke: color }}
        strokeWidth="1.5"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
