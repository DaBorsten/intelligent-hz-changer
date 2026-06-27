import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import type { MonitorHz, MonitorInfoExtended, WatchConfig } from "../types";
import { useTheme } from "../useTheme";

interface SelectOption { value: string; label: string }

function CustomSelect({ value, options, onChange }: {
  value: string;
  options: SelectOption[];
  onChange: (v: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const selected = options.find((o) => o.value === value);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, []);

  return (
    <div ref={ref} className="relative shrink-0 select-none">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className={`
          flex items-center gap-2 px-3 py-2 rounded-xl text-sm font-medium transition-all
          bg-white dark:bg-[#1e1e1e]
          border border-black/10 dark:border-white/8
          text-slate-800 dark:text-slate-100
          hover:bg-slate-50 dark:hover:bg-[#252525]
          hover:border-black/15 dark:hover:border-white/13
          shadow-[0_1px_3px_rgba(0,0,0,0.06)] dark:shadow-[0_1px_4px_rgba(0,0,0,0.4)]
          ${open ? "border-red-400/60 dark:border-red-500/40 ring-2 ring-red-500/10 dark:ring-red-500/10" : ""}
        `}
      >
        <span className="relative">
          <span className="invisible whitespace-nowrap" aria-hidden="true">
            {options.reduce((a, b) => b.label.length > a.label.length ? b : a, options[0]).label}
          </span>
          <span className="absolute inset-0 flex items-center whitespace-nowrap">
            {selected?.label ?? value}
          </span>
        </span>
        <svg
          className={`w-3.5 h-3.5 text-slate-400 dark:text-slate-500 transition-transform duration-200 shrink-0 ${open ? "rotate-180" : ""}`}
          viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg"
        >
          <path d="M2 4l4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/>
        </svg>
      </button>

      {open && (
        <div className="
          absolute right-0 mt-1.5 z-50 min-w-full
          bg-white dark:bg-[#1e1e1e]
          border border-black/10 dark:border-white/8
          rounded-xl overflow-hidden
          shadow-[0_8px_24px_rgba(0,0,0,0.12)] dark:shadow-[0_8px_32px_rgba(0,0,0,0.6)]
        ">
          {options.map((opt) => {
            const isActive = opt.value === value;
            return (
              <button
                key={opt.value}
                type="button"
                onClick={() => { onChange(opt.value); setOpen(false); }}
                className={`
                  w-full flex items-center justify-between gap-2 px-3 py-2.5 text-sm font-medium text-left transition-colors
                  ${isActive
                    ? "bg-red-500/8 dark:bg-red-500/10 text-red-600 dark:text-red-400"
                    : "text-slate-700 dark:text-slate-200 hover:bg-slate-50 dark:hover:bg-white/5"
                  }
                `}
              >
                <span className="whitespace-nowrap">{opt.label}</span>
                {isActive && (
                  <svg className="w-3.5 h-3.5 shrink-0" viewBox="0 0 12 12" fill="none">
                    <path d="M2 6l3 3 5-5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/>
                  </svg>
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

interface Props {
  config: WatchConfig;
  onChange: (partial: Partial<WatchConfig>) => void;
  onSave: (override?: Partial<WatchConfig>) => void;
  saving: boolean;
}

function extractDisplayNum(deviceName: string): number {
  const m = deviceName.match(/DISPLAY(\d+)/i);
  return m ? parseInt(m[1]) : 0;
}

function getMonitorLabel(mon: MonitorInfoExtended, all: MonitorInfoExtended[], t: (k: string) => string): string {
  if (mon.is_primary) return t("monitor.labelPrimary");
  if (mon.is_duplicate) return t("monitor.labelClone");
  const secondary = all.filter((m) => !m.is_primary && !m.is_duplicate);
  if (secondary.length === 1) return t("monitor.labelSide");
  const primary = all.find((m) => m.is_primary);
  if (!primary) return t("monitor.labelSide");
  const dx = mon.x - primary.x;
  if (dx > 100) return t("monitor.labelRight");
  if (dx < -100) return t("monitor.labelLeft");
  return mon.y < primary.y ? t("monitor.labelAbove") : t("monitor.labelBelow");
}

const CANVAS_H = 320;
const CANVAS_PAD = 28;


export function MonitorConfig({ config, onChange, onSave, saving }: Props) {
  const { t } = useTranslation();
  const { isDark } = useTheme();
  const [monitors, setMonitors] = useState<MonitorInfoExtended[]>([]);
  const [supportedHz, setSupportedHz] = useState<number[]>([]);
  const [loading, setLoading] = useState(false);
  const [testing, setTesting] = useState(false);
  const [saveMsg, setSaveMsg] = useState("");
  const canvasRef = useRef<HTMLDivElement>(null);
  const [canvasWidth, setCanvasWidth] = useState(640);
  const [draftGameHz, setDraftGameHz] = useState(config.game_hz);
  const [draftDefaultHz, setDraftDefaultHz] = useState(config.default_hz);
  const [savedGameHz, setSavedGameHz] = useState(config.game_hz);
  const [savedDefaultHz, setSavedDefaultHz] = useState(config.default_hz);
  const isDirty = draftGameHz !== savedGameHz || draftDefaultHz !== savedDefaultHz;
  const draftGameHzRef = useRef(draftGameHz);
  const draftDefaultHzRef = useRef(draftDefaultHz);
  const isDirtyRef = useRef(isDirty);

  useEffect(() => {
    draftGameHzRef.current = draftGameHz;
    draftDefaultHzRef.current = draftDefaultHz;
    isDirtyRef.current = isDirty;
  }, [draftDefaultHz, draftGameHz, isDirty]);

  useEffect(() => {
    invoke<MonitorInfoExtended[]>("get_monitors_extended")
      .then((mons) => {
        setMonitors(mons);
        if (!config.monitor_name && mons.length > 0) {
          const initial = mons.find((m) => m.is_primary) ?? mons[0];
          onChange({ monitor_name: initial.device_name });
        }
      })
      .catch(console.error);
  }, [config.monitor_name, onChange]);

  const prevMonitorRef = useRef<string>("");

  useEffect(() => {
    if (!config.monitor_name) return;
    let cancelled = false;
    const monitorChanged = prevMonitorRef.current !== config.monitor_name;
    prevMonitorRef.current = config.monitor_name;
    queueMicrotask(() => {
      if (!cancelled) setLoading(true);
    });
    void invoke<number[]>("get_supported_hz", { monitorName: config.monitor_name })
      .then((hz) => {
        if (cancelled) return;
        setSupportedHz(hz);
        if (hz.length === 0) return;
        if (monitorChanged || !isDirtyRef.current) {
          const saved = config.monitor_settings[config.monitor_name] as MonitorHz | undefined;
          const newGameHz = saved?.game_hz && hz.includes(saved.game_hz) ? saved.game_hz : hz[hz.length - 1];
          const newDefaultHz = saved?.default_hz && hz.includes(saved.default_hz) ? saved.default_hz : (hz.includes(60) ? 60 : hz[0]);
          setDraftGameHz(newGameHz);
          setDraftDefaultHz(newDefaultHz);
          setSavedGameHz(newGameHz);
          setSavedDefaultHz(newDefaultHz);
        } else {
          if (!hz.includes(draftGameHzRef.current)) setDraftGameHz(hz[hz.length - 1]);
          if (!hz.includes(draftDefaultHzRef.current)) setDraftDefaultHz(hz.includes(60) ? 60 : hz[0]);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [config.monitor_name, config.monitor_settings]);

  useEffect(() => {
    if (!canvasRef.current) return;
    const obs = new ResizeObserver((entries) => {
      setCanvasWidth(entries[0].contentRect.width);
    });
    obs.observe(canvasRef.current);
    return () => obs.disconnect();
  }, []);

  const layout = computeLayout(monitors, canvasWidth, CANVAS_H, CANVAS_PAD);
  const configuredMonitor = monitors.find((m) => m.device_name === config.monitor_name);

  const hzWarning =
    supportedHz.length > 0 && draftGameHz < draftDefaultHz
      ? t("monitor.hzWarning")
      : null;

  async function handleTestHz() {
    if (!config.monitor_name) return;
    setTesting(true);
    try {
      await invoke("test_hz", { monitorName: config.monitor_name, hz: draftGameHz });
      setTimeout(() => setTesting(false), 5500);
    } catch {
      setTesting(false);
    }
  }

  function handleSave() {
    const settings = { ...config.monitor_settings };
    settings[config.monitor_name] = { game_hz: draftGameHz, default_hz: draftDefaultHz };
    const override = { game_hz: draftGameHz, default_hz: draftDefaultHz, monitor_settings: settings };
    onChange(override);
    onSave(override);
    setSavedGameHz(draftGameHz);
    setSavedDefaultHz(draftDefaultHz);
    setSaveMsg(t("monitor.saved"));
    setTimeout(() => setSaveMsg(""), 2500);
  }

  const hzButtons = supportedHz.length > 0 ? supportedHz : [60, 75, 90, 120, 144, 165, 200, 240];

  return (
    <div className="space-y-4">
      {/* Visual monitor canvas */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-4">
        <div
          ref={canvasRef}
          className="relative rounded-xl overflow-hidden"
          style={{
            height: CANVAS_H,
            backgroundColor: "transparent",
            backgroundImage: "radial-gradient(circle, #cbd5e1 1px, transparent 1px)",
            backgroundSize: "20px 20px",
          }}
        >
          {layout.map(({ mon, left, top, w, h, displayNum, isCloneGroup, groupDeviceNames }) => {
            const isConfigured = groupDeviceNames.includes(config.monitor_name);
            const label = isCloneGroup ? t("monitor.labelClone") : getMonitorLabel(mon, monitors, t);

            return (
              <div
                key={mon.device_name}
                onClick={() => onChange({ monitor_name: mon.device_name })}
                className="absolute rounded-xl cursor-pointer transition-[background-color,border-color,box-shadow] select-none flex flex-col monitor-card"
                style={{
                  left, top, width: w, height: h,
                  backgroundColor: isConfigured ? "#ef4444" : isDark ? "#2a2a2a" : "white",
                  border: `2px solid ${isConfigured ? "#dc2626" : isDark ? "#3f3f3f" : "#e2e8f0"}`,
                  boxShadow: isConfigured
                    ? "0 4px 24px rgba(239,68,68,0.35)"
                    : isDark ? "0 2px 8px rgba(0,0,0,0.4)" : "0 2px 8px rgba(0,0,0,0.07)",
                }}
              >
                <div className="flex items-center justify-between px-2 pt-1.5 shrink-0">
                  <span
                    className="font-bold tracking-widest"
                    style={{
                      fontSize: Math.max(9, Math.min(12, h * 0.06)),
                      color: isConfigured ? "rgba(255,255,255,0.8)" : isDark ? "#64748b" : "#94a3b8",
                    }}
                  >
                    {label}
                  </span>
                  {isConfigured && (
                    <span
                      className="font-bold rounded-full px-1.5 py-0.5"
                      style={{ fontSize: 7, backgroundColor: "rgba(255,255,255,0.25)", color: "white" }}
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
                      color: isConfigured ? "white" : isDark ? "#cbd5e1" : "#1e293b",
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
                      color: isConfigured ? "rgba(255,255,255,0.7)" : isDark ? "#64748b" : "#94a3b8",
                    }}
                  >
                    {mon.width} × {mon.height} · {mon.max_hz}Hz
                  </span>
                </div>
              </div>
            );
          })}
        </div>

        <div className="flex items-center gap-3 mt-3">
          <button
            onClick={() => void invoke("identify_monitors", { theme: isDark ? "dark" : "light" }).catch(console.error)}
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

      {/* Monitor detail card */}
      {configuredMonitor && (
        <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-4">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-3 min-w-0 flex-1">
              <div className="w-8 h-8 rounded-full bg-red-500 flex items-center justify-center text-white font-bold shrink-0 shadow-sm shadow-red-500/30"
                style={{ fontSize: layout.find((l) => l.groupDeviceNames.includes(config.monitor_name))?.isCloneGroup ? 10 : 14 }}>
                {layout.find((l) => l.groupDeviceNames.includes(config.monitor_name))?.displayNum ?? extractDisplayNum(configuredMonitor.device_name)}
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-sm font-semibold text-slate-900 dark:text-slate-100 truncate select-text">
                  {configuredMonitor.friendly_name}
                </div>
                <div className="text-xs text-slate-400 dark:text-slate-500 font-mono truncate select-text">
                  {configuredMonitor.width} × {configuredMonitor.height} · {t("monitor.upTo", { hz: configuredMonitor.max_hz })}
                </div>
              </div>
            </div>
            {monitors.length > 1 && (
              <CustomSelect
                value={config.monitor_name}
                options={monitors.map((m) => ({
                  value: m.device_name,
                  label: `${getMonitorLabel(m, monitors, t)} — ${m.friendly_name}`,
                }))}
                onChange={(v) => onChange({ monitor_name: v })}
              />
            )}
          </div>
        </div>
      )}

      {/* Hz selectors */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-4 space-y-5">
        <div>
          <div className="flex items-center gap-2 mb-3">
            <span className="w-2.5 h-2.5 rounded-full bg-red-500 shrink-0" />
            <span className="text-sm font-semibold text-slate-800 dark:text-slate-100">{t("monitor.gameHz")}</span>
            <span className="text-xs text-slate-400 dark:text-slate-500">{t("monitor.gameHzHint")}</span>
          </div>
          <div className="flex flex-wrap gap-2">
            {hzButtons.map((hz) => (
              <button
                key={hz}
                onClick={() => setDraftGameHz(hz)}
                disabled={loading}
                className={`px-3.5 py-1.5 rounded-xl text-sm font-semibold transition-all ${
                  draftGameHz === hz
                    ? "bg-red-500 text-white shadow-sm shadow-red-500/30 border border-transparent"
                    : "bg-white dark:bg-[#2a2a2a] text-slate-600 dark:text-slate-300 border border-black/8 dark:border-white/8 hover:border-black/20 dark:hover:border-white/20"
                }`}
              >
                {hz} Hz
              </button>
            ))}
          </div>
        </div>

        <div className="border-t border-black/6 dark:border-white/6" />

        <div>
          <div className="flex items-center gap-2 mb-3">
            <span className="w-2.5 h-2.5 rounded-full bg-slate-400 dark:bg-slate-500 shrink-0" />
            <span className="text-sm font-semibold text-slate-800 dark:text-slate-100">{t("monitor.defaultHz")}</span>
            <span className="text-xs text-slate-400 dark:text-slate-500">{t("monitor.defaultHzHint")}</span>
          </div>
          <div className="flex flex-wrap gap-2">
            {hzButtons.map((hz) => (
              <button
                key={hz}
                onClick={() => setDraftDefaultHz(hz)}
                disabled={loading}
                className={`px-3.5 py-1.5 rounded-xl text-sm font-semibold transition-all ${
                  draftDefaultHz === hz
                    ? "bg-slate-700 dark:bg-slate-200 text-white dark:text-slate-900 border border-transparent"
                    : "bg-white dark:bg-[#2a2a2a] text-slate-600 dark:text-slate-300 border border-black/8 dark:border-white/8 hover:border-black/20 dark:hover:border-white/20"
                }`}
              >
                {hz} Hz
              </button>
            ))}
          </div>
        </div>
      </div>

      {hzWarning && (
        <div className="bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-700 rounded-xl px-4 py-2.5 text-sm text-amber-700 dark:text-amber-400">
          {hzWarning}
        </div>
      )}

      {/* Actions */}
      <div className="sticky bottom-5 flex justify-end">
        <div className="flex items-center gap-2 bg-white dark:bg-[#1c1c1c] border border-black/8 dark:border-white/8 rounded-2xl shadow-lg px-2 py-2">
          {saveMsg && <span className="text-xs text-slate-500 dark:text-slate-400 font-medium px-1">{saveMsg}</span>}
          <button
            onClick={() => void handleTestHz()}
            disabled={testing || !config.monitor_name || !isDirty}
            className="px-4 py-2 bg-[#f0eeeb] dark:bg-[#2a2a2a] border border-black/8 dark:border-white/10 text-slate-700 dark:text-slate-300 text-sm font-medium
                       rounded-xl hover:bg-slate-200 dark:hover:bg-[#333] disabled:opacity-40 transition-colors"
          >
            {testing ? t("monitor.testing") : t("monitor.testBtn")}
          </button>
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
  canvasW: number,
  canvasH: number,
  pad: number
): LayoutItem[] {
  if (monitors.length === 0) return [];

  const posKey = (m: MonitorInfoExtended) => `${m.x},${m.y},${m.width},${m.height}`;
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
    const nums = group.map((m) => extractDisplayNum(m.device_name)).join("|");
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
