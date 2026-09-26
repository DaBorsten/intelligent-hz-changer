import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  buildDisplayNums,
  fallbackDefaultHz,
  fallbackGameHz,
  getMonitorLabel,
} from "../monitors";
import type {
  BackendStatus,
  MonitorInfoExtended,
  MonitorProfile,
  WatchConfig,
} from "../types";
import { useTheme } from "../useTheme";
import { CustomSelect } from "./CustomSelect";
import { Switch } from "./Switch";

interface Props {
  config: WatchConfig;
  onChange: (partial: Partial<WatchConfig>) => void;
  onSave: (override?: Partial<WatchConfig>) => void;
  saving: boolean;
}

type Profiles = Record<string, MonitorProfile>;

const FALLBACK_HZ = [60, 75, 90, 120, 144, 165, 200, 240];

/**
 * The saved profiles plus a disabled one for every connected monitor that has
 * none yet, so each card has values to show. On a fresh install nothing is
 * saved, and the draft switches the primary monitor on — the old
 * single-monitor default — which leaves Save enabled to confirm it.
 */
function buildProfiles(
  saved: Profiles,
  monitors: MonitorInfoExtended[],
  supported: Record<string, number[]>,
): { baseline: Profiles; draft: Profiles } {
  const baseline: Profiles = { ...saved };
  for (const m of monitors) {
    if (baseline[m.device_name]) continue;
    const hz = supported[m.device_name] ?? [];
    baseline[m.device_name] = {
      enabled: false,
      game_hz: fallbackGameHz(hz, m),
      default_hz: fallbackDefaultHz(hz, m),
    };
  }
  const draft = { ...baseline };
  if (Object.keys(saved).length === 0 && monitors.length > 0) {
    const primary = monitors.find((m) => m.is_primary) ?? monitors[0];
    draft[primary.device_name] = {
      ...draft[primary.device_name],
      enabled: true,
    };
  }
  return { baseline, draft };
}

const CANVAS_H = 320;
const CANVAS_PAD = 28;

export function MonitorConfig({ config, onChange, onSave, saving }: Props) {
  const { t } = useTranslation();
  const { isDark } = useTheme();
  const [monitors, setMonitors] = useState<MonitorInfoExtended[]>([]);
  const [supportedHz, setSupportedHz] = useState<Record<string, number[]>>({});
  const [loaded, setLoaded] = useState(false);
  // Device name of the monitor whose test is running.
  const [testing, setTesting] = useState<string | null>(null);
  // How long a test lasts is a backend question: on GNOME the trial runs inside
  // Mutter's own "keep these settings?" prompt, which counts down for 20 s.
  const [testSeconds, setTestSeconds] = useState(5);
  const [backend, setBackend] = useState<BackendStatus | null>(null);
  const [testError, setTestError] = useState<{
    hz: number;
    error: string;
  } | null>(null);
  const [saveMsg, setSaveMsg] = useState("");
  const canvasRef = useRef<HTMLDivElement>(null);
  const [canvasWidth, setCanvasWidth] = useState(640);
  const [draft, setDraft] = useState<Profiles>({});
  const [baseline, setBaseline] = useState<Profiles>({});
  // Both come out of `buildProfiles` or a save, with keys in the same order,
  // so comparing their JSON is a structural comparison.
  const isDirty = JSON.stringify(draft) !== JSON.stringify(baseline);
  const isDirtyRef = useRef(isDirty);
  useEffect(() => {
    isDirtyRef.current = isDirty;
  }, [isDirty]);

  // Enumerating monitors builds a COM/WMI connection and walks every display
  // mode, so it runs once on mount rather than on every config change.
  useEffect(() => {
    invoke<number>("get_test_seconds")
      .then(setTestSeconds)
      .catch(console.error);
    invoke<BackendStatus>("get_display_backend_status")
      .then(setBackend)
      .catch(console.error);
    invoke<MonitorInfoExtended[]>("get_monitors_extended")
      .then(async (mons) => {
        const rates = await Promise.all(
          mons.map((m) =>
            invoke<number[]>("get_supported_hz", {
              monitorName: m.device_name,
            }).catch(() => [] as number[]),
          ),
        );
        setSupportedHz(
          Object.fromEntries(mons.map((m, i) => [m.device_name, rates[i]])),
        );
        setMonitors(mons);
        setLoaded(true);
      })
      .catch(console.error);
  }, []);

  // (Re)build the draft once the hardware is known, and whenever the saved
  // profiles change underneath an untouched draft (config loading late, or a
  // game's override creating a profile).
  useEffect(() => {
    if (!loaded || isDirtyRef.current) return;
    const next = buildProfiles(config.monitors, monitors, supportedHz);
    queueMicrotask(() => {
      setBaseline(next.baseline);
      setDraft(next.draft);
    });
  }, [loaded, config.monitors, monitors, supportedHz]);

  useEffect(() => {
    if (!canvasRef.current) return;
    const obs = new ResizeObserver((entries) => {
      setCanvasWidth(entries[0].contentRect.width);
    });
    obs.observe(canvasRef.current);
    return () => obs.disconnect();
  }, []);

  const displayNums = buildDisplayNums(monitors);
  const layout = computeLayout(
    monitors,
    displayNums,
    canvasWidth,
    CANVAS_H,
    CANVAS_PAD,
  );
  const sortedMonitors = [...monitors].sort(
    (a, b) =>
      (displayNums.get(a.device_name) ?? 0) -
      (displayNums.get(b.device_name) ?? 0),
  );
  const connected = new Set(monitors.map((m) => m.device_name));
  const disconnected = Object.keys(draft).filter(
    (name) => !connected.has(name) && draft[name].enabled,
  );

  function patchProfile(names: string[], patch: Partial<MonitorProfile>) {
    setDraft((prev) => {
      const next = { ...prev };
      for (const n of names) if (next[n]) next[n] = { ...next[n], ...patch };
      return next;
    });
  }

  async function handleTestHz(monitorName: string, hz: number) {
    setTesting(monitorName);
    setTestError(null);
    try {
      await invoke("test_hz", { monitorName, hz });
      setTimeout(() => setTesting(null), testSeconds * 1000 + 500);
    } catch (e) {
      // The rate the backend refused here is the one a save would apply too, so
      // the reason has to be visible rather than just ending the test silently.
      setTestError({ hz, error: String(e) });
      setTesting(null);
    }
  }

  function handleSave() {
    const override = { monitors: draft };
    onChange(override);
    onSave(override);
    setBaseline(draft);
    setSaveMsg(t("monitor.saved"));
    setTimeout(() => setSaveMsg(""), 2500);
  }

  function hzOptions(deviceName: string, current: number) {
    const supported = supportedHz[deviceName];
    const list = supported && supported.length > 0 ? supported : FALLBACK_HZ;
    const all = list.includes(current) ? list : [...list, current];
    return [...all]
      .sort((a, b) => b - a)
      .map((hz) => ({ value: String(hz), label: `${hz} Hz` }));
  }

  // An unusable backend produces an empty monitor list, which on its own looks
  // like "no displays attached" — so say what actually went wrong. The generic
  // fallback key covers codes a newer backend may add.
  const backendError =
    backend && !backend.ok
      ? t(
          [
            `monitor.backendError.${backend.code}`,
            "monitor.backendError.generic",
          ],
          { detail: backend.detail },
        )
      : null;

  return (
    <div className="space-y-4">
      {backendError && (
        <div className="rounded-xl border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/25 px-4 py-3">
          <div className="text-sm font-semibold text-red-700 dark:text-red-300">
            {t("monitor.backendError.title")}
          </div>
          <div className="text-xs text-red-600/90 dark:text-red-400/90 mt-0.5">
            {backendError}
          </div>
        </div>
      )}

      {/* Visual monitor canvas */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-4">
        <div
          ref={canvasRef}
          className="relative rounded-xl overflow-hidden"
          style={{
            height: CANVAS_H,
            backgroundColor: "transparent",
            backgroundImage:
              "radial-gradient(circle, #cbd5e1 1px, transparent 1px)",
            backgroundSize: "20px 20px",
          }}
        >
          {layout.map(
            ({
              mon,
              left,
              top,
              w,
              h,
              displayNum,
              isCloneGroup,
              groupDeviceNames,
            }) => {
              const isOn = groupDeviceNames.some((n) => draft[n]?.enabled);
              const label = isCloneGroup
                ? t("monitor.labelClone")
                : getMonitorLabel(mon, monitors, t);

              const on = {
                bg: isDark ? "#a83838" : "#d64545",
                border: isDark ? "#bd4545" : "#c23a3a",
                shadow: isDark
                  ? "0 4px 16px rgba(0,0,0,0.45)"
                  : "0 4px 16px rgba(214,69,69,0.2)",
                label: "rgba(255,255,255,0.75)",
                num: "white",
                sub: "rgba(255,255,255,0.7)",
                badgeBg: "rgba(255,255,255,0.2)",
                badgeText: "white",
              };

              return (
                <div
                  key={mon.device_name}
                  onClick={() =>
                    patchProfile(groupDeviceNames, { enabled: !isOn })
                  }
                  className="absolute rounded-xl cursor-pointer transition-[background-color,border-color,box-shadow] select-none flex flex-col monitor-card"
                  style={{
                    left,
                    top,
                    width: w,
                    height: h,
                    backgroundColor: isOn
                      ? on.bg
                      : isDark
                        ? "#2a2a2a"
                        : "white",
                    border: `2px solid ${isOn ? on.border : isDark ? "#3f3f3f" : "#e2e8f0"}`,
                    boxShadow: isOn
                      ? on.shadow
                      : isDark
                        ? "0 2px 8px rgba(0,0,0,0.4)"
                        : "0 2px 8px rgba(0,0,0,0.07)",
                  }}
                >
                  <div className="flex items-center justify-between px-2 pt-1.5 shrink-0">
                    <span
                      className="font-bold tracking-widest"
                      style={{
                        fontSize: Math.max(9, Math.min(12, h * 0.06)),
                        color: isOn
                          ? on.label
                          : isDark
                            ? "#64748b"
                            : "#94a3b8",
                      }}
                    >
                      {label}
                    </span>
                    {isOn && (
                      <span
                        className="font-bold rounded-full px-1.5 py-0.5"
                        style={{
                          fontSize: 7,
                          backgroundColor: on.badgeBg,
                          color: on.badgeText,
                        }}
                      >
                        {t("monitor.activeBadge")}
                      </span>
                    )}
                  </div>

                  <div className="flex-1 flex items-center justify-center">
                    <span
                      className="font-black leading-none"
                      style={{
                        fontSize: Math.min(h * 0.42, isCloneGroup ? 52 : 72),
                        color: isOn ? on.num : isDark ? "#cbd5e1" : "#1e293b",
                        letterSpacing: isCloneGroup ? "-0.02em" : undefined,
                      }}
                    >
                      {displayNum}
                    </span>
                  </div>

                  <div className="text-center pb-2 shrink-0">
                    <span
                      style={{
                        fontSize: Math.max(9, Math.min(12, h * 0.055)),
                        color: isOn
                          ? on.sub
                          : isDark
                            ? "#64748b"
                            : "#94a3b8",
                      }}
                    >
                      {mon.width} × {mon.height} · {mon.max_hz}Hz
                    </span>
                  </div>
                </div>
              );
            },
          )}
        </div>

        <div className="flex items-center gap-3 mt-3">
          <button
            onClick={() =>
              void invoke("identify_monitors", {
                theme: isDark ? "dark" : "light",
              }).catch(console.error)
            }
            className="px-3 py-1.5 bg-white dark:bg-[#2a2a2a] border border-black/10 dark:border-white/10 rounded-lg text-xs font-medium
                       text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-[#333] transition-colors shadow-sm"
          >
            {t("monitor.identify")}
          </button>
          <span className="text-xs text-slate-400 dark:text-slate-500">
            {t("monitor.clickHint")}
          </span>
        </div>
      </div>

      {/* Per-monitor global settings */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-4 space-y-3">
        <div className="flex items-baseline gap-2">
          <h2 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
            {t("monitor.globalTitle")}
          </h2>
          <span className="text-xs text-slate-400 dark:text-slate-500">
            {t("monitor.globalHint")}
          </span>
        </div>

        {sortedMonitors.map((mon) => {
          const profile = draft[mon.device_name];
          if (!profile) return null;
          const num = displayNums.get(mon.device_name);
          const isTesting = testing === mon.device_name;
          return (
            <div
              key={mon.device_name}
              className="rounded-xl border border-black/6 dark:border-white/6 bg-white dark:bg-[#2a2a2a] p-3 space-y-3"
            >
              <div className="flex items-center gap-3">
                <div
                  className={`w-8 h-8 rounded-full flex items-center justify-center font-bold shrink-0 text-sm ${
                    profile.enabled
                      ? "bg-red-500 text-white shadow-sm shadow-red-500/30"
                      : "bg-slate-200 dark:bg-slate-700 text-slate-500 dark:text-slate-300"
                  }`}
                >
                  {num}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-semibold text-slate-900 dark:text-slate-100 truncate select-text">
                    {mon.friendly_name}
                  </div>
                  <div className="text-xs text-slate-400 dark:text-slate-500 font-mono truncate select-text">
                    {mon.width} × {mon.height} ·{" "}
                    {t("monitor.upTo", { hz: mon.max_hz })}
                  </div>
                </div>
                <span className="text-xs text-slate-500 dark:text-slate-400 shrink-0">
                  {t(
                    profile.enabled ? "monitor.switchOn" : "monitor.switchOff",
                  )}
                </span>
                <Switch
                  checked={profile.enabled}
                  onChange={(v) =>
                    patchProfile([mon.device_name], { enabled: v })
                  }
                  title={t("monitor.switchTitle")}
                />
              </div>

              <div className="flex flex-wrap items-end gap-3">
                <div className="space-y-1">
                  <span
                    className={`flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400 ${profile.enabled ? "" : "opacity-60"}`}
                  >
                    <span className="w-2 h-2 rounded-full bg-red-500" />
                    {t("monitor.gameHz")}
                  </span>
                  <CustomSelect
                    value={String(profile.game_hz)}
                    options={hzOptions(mon.device_name, profile.game_hz)}
                    onChange={(v) =>
                      patchProfile([mon.device_name], { game_hz: Number(v) })
                    }
                    dimmed={!profile.enabled}
                  />
                </div>
                <div className="space-y-1">
                  <span
                    className={`flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400 ${profile.enabled ? "" : "opacity-60"}`}
                  >
                    <span className="w-2 h-2 rounded-full bg-slate-400 dark:bg-slate-500" />
                    {t("monitor.defaultHz")}
                  </span>
                  <CustomSelect
                    value={String(profile.default_hz)}
                    options={hzOptions(mon.device_name, profile.default_hz)}
                    onChange={(v) =>
                      patchProfile([mon.device_name], {
                        default_hz: Number(v),
                      })
                    }
                    dimmed={!profile.enabled}
                  />
                </div>
                <button
                  onClick={() =>
                    void handleTestHz(mon.device_name, profile.game_hz)
                  }
                  disabled={testing !== null}
                  className={`${profile.enabled ? "" : "opacity-60"} px-3 py-2 bg-[#f0eeeb] dark:bg-[#1e1e1e] border border-black/8 dark:border-white/10 text-slate-700 dark:text-slate-300 text-sm font-medium
                             rounded-xl hover:bg-slate-200 dark:hover:bg-[#333] disabled:opacity-40 transition-colors`}
                >
                  {isTesting
                    ? t("monitor.testing")
                    : t("monitor.testBtn", { seconds: testSeconds })}
                </button>
              </div>

              {profile.game_hz < profile.default_hz && (
                <div className="bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-700 rounded-lg px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
                  {t("monitor.hzWarning")}
                </div>
              )}
            </div>
          );
        })}

        {disconnected.map((name) => (
          <p
            key={name}
            className="text-xs text-slate-400 dark:text-slate-500 italic"
          >
            {t("monitor.disconnected", { name })}
          </p>
        ))}
      </div>

      {testError && (
        <div className="rounded-xl border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/25 px-4 py-3">
          <div className="text-sm font-semibold text-red-700 dark:text-red-300">
            {t("monitor.testFailed", { hz: testError.hz })}
          </div>
          <div className="text-xs text-red-600/90 dark:text-red-400/90 mt-0.5 wrap-break-word select-text">
            {testError.error}
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="sticky bottom-5 flex justify-end">
        <div className="flex items-center gap-2 bg-white dark:bg-[#1c1c1c] border border-black/8 dark:border-white/8 rounded-2xl shadow-lg px-2 py-2">
          {saveMsg && (
            <span className="text-xs text-slate-500 dark:text-slate-400 font-medium px-1">
              {saveMsg}
            </span>
          )}
          <button
            onClick={handleSave}
            disabled={saving || !isDirty}
            className="flex items-center gap-2 px-5 py-2 bg-slate-800 hover:bg-slate-700 dark:bg-slate-200 dark:hover:bg-slate-300
                       disabled:opacity-50 text-white dark:text-slate-900 text-sm font-semibold rounded-xl transition-colors"
          >
            {saving ? t("monitor.saving") : t("monitor.saveBtn")}
          </button>
        </div>
      </div>
    </div>
  );
}

interface LayoutItem {
  mon: MonitorInfoExtended;
  left: number;
  top: number;
  w: number;
  h: number;
  displayNum: string;
  isCloneGroup: boolean;
  groupDeviceNames: string[];
}

function computeLayout(
  monitors: MonitorInfoExtended[],
  displayNums: Map<string, number>,
  canvasW: number,
  canvasH: number,
  pad: number,
): LayoutItem[] {
  if (monitors.length === 0) return [];

  const posKey = (m: MonitorInfoExtended) =>
    `${m.x},${m.y},${m.width},${m.height}`;
  const seen = new Set<string>();
  const groups: MonitorInfoExtended[][] = [];
  for (const mon of monitors) {
    if (mon.is_duplicate) {
      const k = posKey(mon);
      if (!seen.has(k)) {
        seen.add(k);
        groups.push(monitors.filter((m) => posKey(m) === k));
      }
    } else {
      groups.push([mon]);
    }
  }

  const reps = groups.map((g) => g[0]);
  const minX = Math.min(...reps.map((m) => m.x));
  const minY = Math.min(...reps.map((m) => m.y));
  const maxX = Math.max(...reps.map((m) => m.x + m.width));
  const maxY = Math.max(...reps.map((m) => m.y + m.height));
  const totalW = maxX - minX || 1;
  const totalH = maxY - minY || 1;

  const scaleX = (canvasW - pad * 2) / totalW;
  const scaleY = (canvasH - pad * 2) / totalH;
  const scale = Math.min(scaleX, scaleY) * 0.75;
  const scaledW = totalW * scale;
  const scaledH = totalH * scale;
  const offsetX = (canvasW - scaledW) / 2 - minX * scale;
  const offsetY = (canvasH - scaledH) / 2 - minY * scale;

  return groups.map((group) => {
    const rep = group[0];
    const nums = group.map((m) => displayNums.get(m.device_name)).join("|");
    return {
      mon: rep,
      left: rep.x * scale + offsetX,
      top: rep.y * scale + offsetY,
      w: rep.width * scale,
      h: rep.height * scale,
      displayNum: nums,
      isCloneGroup: group.length > 1,
      groupDeviceNames: group.map((m) => m.device_name),
    };
  });
}
