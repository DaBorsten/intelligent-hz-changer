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
const MONITOR_GAP = 6;

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
    // Measure now so the tiles don't first lay out against the default width.
    setCanvasWidth(canvasRef.current.clientWidth);
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
        <div className="rounded-lg border border-accent-line bg-accent-soft px-4 py-3">
          <div className="text-sm font-semibold text-accent-text">
            {t("monitor.backendError.title")}
          </div>
          <div className="text-xs text-accent-text/90 mt-0.5">
            {backendError}
          </div>
        </div>
      )}

      {/* Visual monitor canvas */}
      <div className="card overflow-hidden">
        <div
          ref={canvasRef}
          className="relative overflow-hidden"
          style={{
            height: CANVAS_H,
            backgroundImage:
              "radial-gradient(var(--line-strong) 1px, transparent 1px)",
            backgroundSize: "18px 18px",
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
              const tinyText = Math.max(9, Math.min(11, h * 0.06));

              return (
                <div
                  key={mon.device_name}
                  onClick={() =>
                    patchProfile(groupDeviceNames, { enabled: !isOn })
                  }
                  className={`hl absolute rounded-[10px] border cursor-pointer select-none flex flex-col justify-between monitor-card ${
                    isOn
                      ? "bg-accent-soft border-accent-line-strong"
                      : "bg-tile border-line-strong"
                  }`}
                  style={{
                    // Adjacent monitors touch; the inset keeps their borders apart.
                    left: left + MONITOR_GAP / 2,
                    top: top + MONITOR_GAP / 2,
                    width: w - MONITOR_GAP,
                    height: h - MONITOR_GAP,
                    padding: h < 100 ? "6px 8px" : "12px 14px",
                    transition:
                      "background-color 200ms var(--ease-out), border-color 200ms var(--ease-out)",
                  }}
                >
                  <div className="flex items-center justify-between gap-1 shrink-0">
                    <span
                      className={`font-semibold tracking-[0.08em] truncate ${isOn ? "text-accent-text" : "text-muted"}`}
                      style={{ fontSize: Math.min(10, tinyText) }}
                    >
                      {label}
                    </span>
                    {isOn && (
                      <span className="text-[10px] font-semibold text-white bg-accent rounded-full px-1.75 py-px shrink-0">
                        {t("monitor.activeBadge")}
                      </span>
                    )}
                  </div>

                  <div
                    className={`text-center font-semibold leading-none ${isOn ? "text-fg" : "text-fg-3"}`}
                    style={{
                      fontSize: Math.min(h * 0.27, isCloneGroup ? 40 : 52),
                    }}
                  >
                    {displayNum}
                  </div>

                  <div
                    className={`text-center font-mono truncate shrink-0 ${isOn ? "text-fg-2" : "text-muted"}`}
                    style={{ fontSize: tinyText }}
                  >
                    {mon.width} × {mon.height} · {mon.max_hz} Hz
                  </div>
                </div>
              );
            },
          )}
        </div>

        <div className="flex items-center gap-3 px-4 py-3 border-t border-line-subtle">
          <button
            onClick={() =>
              void invoke("identify_monitors", {
                theme: isDark ? "dark" : "light",
              }).catch(console.error)
            }
            className="btn hl btn-press"
          >
            {t("monitor.identify")}
          </button>
          <span className="text-xs text-muted">{t("monitor.clickHint")}</span>
        </div>
      </div>

      {/* Per-monitor global settings. No overflow-hidden: selects open past the card. */}
      <div className="card">
        <div className="flex items-baseline flex-wrap gap-2 px-4 py-3.5">
          <h2 className="font-semibold">{t("monitor.globalTitle")}</h2>
          <span className="text-xs text-muted">{t("monitor.globalHint")}</span>
        </div>

        {sortedMonitors.map((mon) => {
          const profile = draft[mon.device_name];
          if (!profile) return null;
          const num = displayNums.get(mon.device_name);
          const isTesting = testing === mon.device_name;
          return (
            <div
              key={mon.device_name}
              className="border-t border-line-subtle px-4 py-3.5 space-y-3.5"
            >
              <div className="flex items-center gap-3">
                <div
                  className={`w-6 h-6 rounded-md flex items-center justify-center font-semibold shrink-0 text-xs ${
                    profile.enabled
                      ? "bg-accent text-white"
                      : "bg-chip border border-line-strong text-muted"
                  }`}
                >
                  {num}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="font-semibold truncate select-text">
                    {mon.friendly_name}
                  </div>
                  <div className="text-[11px] text-muted font-mono mt-0.5 truncate select-text">
                    {mon.width} × {mon.height} ·{" "}
                    {t("monitor.upTo", { hz: mon.max_hz })}
                  </div>
                </div>
                <span className="text-xs text-fg-3 shrink-0">
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

              <div className="flex flex-wrap items-end gap-2">
                <div className="space-y-1.5">
                  <span
                    className={`flex items-center gap-1.5 text-[11px] text-muted ${profile.enabled ? "" : "opacity-60"}`}
                  >
                    <span className="w-1.5 h-1.5 rounded-full bg-accent" />
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
                <div className="space-y-1.5">
                  <span
                    className={`flex items-center gap-1.5 text-[11px] text-muted ${profile.enabled ? "" : "opacity-60"}`}
                  >
                    <span className="w-1.5 h-1.5 rounded-full bg-faint" />
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
                  className={`btn hl btn-press h-8.5 ${profile.enabled ? "" : "opacity-60"}`}
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
            className="text-xs text-muted italic border-t border-line-subtle px-4 py-3"
          >
            {t("monitor.disconnected", { name })}
          </p>
        ))}

        {/* One save covers every monitor above, so it stays reachable while scrolling. */}
        <div className="sticky bottom-0 flex items-center justify-end gap-3 px-4 py-3 border-t border-line-subtle bg-surface rounded-b-[11px]">
          {saveMsg && (
            <span className="text-xs text-muted font-medium">{saveMsg}</span>
          )}
          <button
            onClick={handleSave}
            disabled={saving || !isDirty}
            className="h-8.5 px-4 rounded-lg bg-fg text-surface text-xs font-semibold shadow-ctl hover:opacity-90 disabled:opacity-50 disabled:cursor-default transition-opacity btn-press"
          >
            {saving ? t("monitor.saving") : t("monitor.saveBtn")}
          </button>
        </div>
      </div>

      {testError && (
        <div className="rounded-lg border border-accent-line bg-accent-soft px-4 py-3">
          <div className="text-sm font-semibold text-accent-text">
            {t("monitor.testFailed", { hz: testError.hz })}
          </div>
          <div className="text-xs text-accent-text/90 mt-0.5 wrap-break-word select-text">
            {testError.error}
          </div>
        </div>
      )}
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
