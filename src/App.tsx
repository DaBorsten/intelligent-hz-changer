import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Settings } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MonitorConfig } from "./components/MonitorConfig";
import { ProcessList } from "./components/ProcessList";
import { SettingsTab } from "./components/SettingsTab";
import { StatusView } from "./components/StatusView";
import { statusMonitor, type WatchConfig } from "./types";
import { clearHzError, useHzStatus } from "./useHzStatus";

type Tab = "status" | "processes" | "monitor" | "settings";

const DEFAULT_CONFIG: WatchConfig = {
  watched_processes: [],
  monitors: {},
  disabled_processes: [],
  process_overrides: {},
};

function PulseIcon({ active }: { active?: boolean }) {
  return (
    <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
      <path
        d="M1 7.5h2.5l2-5 3 10 2-7 1.5 4 1.5-2H14"
        stroke={active ? "#ef4444" : "currentColor"}
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function ListIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
      <line
        x1="1.5"
        y1="4"
        x2="13.5"
        y2="4"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <line
        x1="1.5"
        y1="7.5"
        x2="13.5"
        y2="7.5"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <line
        x1="1.5"
        y1="11"
        x2="9"
        y2="11"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

function MonitorIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
      <rect
        x="1"
        y="2"
        width="13"
        height="9"
        rx="1.5"
        stroke="currentColor"
        strokeWidth="1.4"
      />
      <path
        d="M5 13h5M7.5 11v2"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

function SettingsIcon() {
  return <Settings size={15} />;
}

function Toggle({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex items-center w-11 h-6 rounded-full shrink-0 btn-press ${
        checked ? "bg-red-500" : "bg-slate-300 dark:bg-slate-600"
      }`}
      style={{
        transition:
          "background-color 200ms cubic-bezier(0.23, 1, 0.32, 1), transform 140ms cubic-bezier(0.23, 1, 0.32, 1)",
      }}
    >
      <span
        className="inline-block w-4 h-4 bg-white rounded-full shadow"
        style={{
          transform: checked ? "translateX(1.5rem)" : "translateX(0.25rem)",
          transition: "transform 220ms cubic-bezier(0.34, 1.56, 0.64, 1)",
          boxShadow: "0 1px 4px rgba(0,0,0,0.25)",
        }}
      />
    </button>
  );
}

export default function App() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("status");
  const [config, setConfig] = useState<WatchConfig>(DEFAULT_CONFIG);
  const [saving, setSaving] = useState(false);
  const [active, setActive] = useState(true);

  // Single source of truth for Hz + running set, shared with StatusView so the
  // header pill and the status badge can never disagree.
  const {
    currentHz: headerHz,
    running,
    lastError,
  } = useHzStatus(statusMonitor(config));
  const gameMode = active && running.length > 0;

  useEffect(() => {
    invoke<WatchConfig>("load_config").then(setConfig).catch(console.error);

    invoke<boolean>("get_enabled")
      .then(setActive)
      .catch(() => undefined);

    const unlistenEnabled = listen<boolean>("enabled-changed", (e) => {
      setActive(e.payload);
    });

    return () => {
      void unlistenEnabled.then((fn) => fn());
    };
  }, []);

  const save = useCallback(async (overrideConfig?: Partial<WatchConfig>) => {
    setSaving(true);
    try {
      // Read from the state setter so a save can never persist a stale
      // snapshot captured when this callback was created.
      const merged = await new Promise<WatchConfig>((resolve) => {
        setConfig((prev) => {
          const next = { ...prev, ...overrideConfig };
          resolve(next);
          return prev;
        });
      });
      await invoke("save_config", { config: merged });
    } catch (e) {
      console.error(e);
    } finally {
      setSaving(false);
    }
  }, []);

  // Stable identity: MonitorConfig takes this as an effect dependency, and a new
  // reference each render would re-run a COM/WMI monitor enumeration every time.
  const patchConfig = useCallback(
    (partial: Partial<WatchConfig>, autoSave?: boolean) => {
      setConfig((prev) => ({ ...prev, ...partial }));
      if (autoSave) void save(partial);
    },
    [save],
  );

  const tabs: { id: Tab; label: string; icon: React.ReactNode }[] = [
    {
      id: "status",
      label: t("tabs.status"),
      icon: <PulseIcon active={tab === "status"} />,
    },
    { id: "processes", label: t("tabs.processes"), icon: <ListIcon /> },
    { id: "monitor", label: t("tabs.monitor"), icon: <MonitorIcon /> },
    { id: "settings", label: t("tabs.settings"), icon: <SettingsIcon /> },
  ];

  return (
    <div className="h-screen bg-[#f0eeeb] dark:bg-[#141414] flex flex-col overflow-hidden">
      {/* Header */}
      <header className="bg-white dark:bg-[#1c1c1c] border-b border-black/8 dark:border-white/8 px-5 py-3.5 flex items-center gap-3 shrink-0">
        <div className="w-10 h-10 shrink-0 flex items-center justify-center">
          <img
            src="/intelligent-hz-changer.svg"
            alt="Logo"
            className="w-10 h-10"
          />
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-bold text-slate-900 dark:text-slate-100 leading-tight">
            Intelligent Hz Changer
          </div>
          <div className="text-xs text-slate-400 dark:text-slate-500 leading-tight">
            {t("app.subtitle")}
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <span className="text-sm font-medium text-slate-700 dark:text-slate-300">
            {t("app.active")}
          </span>
          <Toggle
            checked={active}
            onChange={(v) => {
              setActive(v);
              invoke("set_enabled", { value: v }).catch(console.error);
            }}
          />
        </div>
      </header>

      {/* Tab bar */}
      <nav className="bg-[#f0eeeb] dark:bg-[#141414] px-4 pt-2 pb-0 flex items-center gap-1 shrink-0">
        <div className="flex gap-1 flex-1">
          {tabs.map((t) => (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              className={`flex items-center gap-1.5 px-4 py-2 rounded-t-xl text-sm font-medium border-t border-x focus:outline-none btn-press ${
                tab === t.id
                  ? "bg-white dark:bg-[#1c1c1c] text-slate-900 dark:text-slate-100 border-black/8 dark:border-white/8 border-b-white dark:border-b-[#1c1c1c]"
                  : "text-slate-500 dark:text-slate-400 border-transparent hover:text-slate-700 dark:hover:text-slate-200"
              }`}
              style={{
                transition:
                  "color 150ms cubic-bezier(0.23, 1, 0.32, 1), transform 140ms cubic-bezier(0.23, 1, 0.32, 1)",
              }}
            >
              {t.icon}
              {t.label}
            </button>
          ))}
        </div>
        {/* Hz status pill */}
        <div className="flex items-center gap-1.5 pb-1">
          <span
            className={`w-2 h-2 rounded-full shrink-0 ${gameMode ? "bg-red-500 dot-pulse" : "bg-slate-400 dark:bg-slate-500"}`}
          />
          <span
            key={String(gameMode)}
            className="text-xs font-medium text-slate-600 dark:text-slate-300 whitespace-nowrap badge-anim"
          >
            {t(gameMode ? "mode.game" : "mode.standard")}
          </span>
          {headerHz != null && (
            <span
              key={headerHz}
              className="text-xs font-bold text-slate-700 dark:text-slate-200 ml-0.5 badge-anim"
            >
              {headerHz} Hz
            </span>
          )}
        </div>
      </nav>

      {/* Content */}
      <main className="flex-1 overflow-y-auto bg-white dark:bg-[#1c1c1c] border-t border-black/8 dark:border-white/8">
        {/* A refused Hz switch leaves the monitor on the wrong rate, which is
            invisible otherwise — so it is shown on every tab, not just the
            monitor one. */}
        {lastError && (
          <div className="mx-5 mt-5 flex items-start gap-3 rounded-xl border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/25 px-4 py-3">
            <div className="flex-1 min-w-0">
              <div className="text-sm font-semibold text-red-700 dark:text-red-300">
                {t("app.hzErrorTitle", { hz: lastError.target_hz })}
              </div>
              <div className="text-xs text-red-600/90 dark:text-red-400/90 mt-0.5 wrap-break-word select-text">
                {lastError.error}
              </div>
            </div>
            <button
              onClick={clearHzError}
              aria-label={t("app.dismiss")}
              className="shrink-0 text-red-500 dark:text-red-400 hover:text-red-700 dark:hover:text-red-200 text-lg leading-none px-1"
            >
              ×
            </button>
          </div>
        )}
        <div className="p-5">
          <div key={`tab-${tab}`} className="tab-content">
            {tab === "status" && (
              <StatusView
                monitorName={statusMonitor(config)}
                watchedProcesses={config.watched_processes}
                gameHz={config.monitors[statusMonitor(config)]?.game_hz}
                active={active}
              />
            )}
            {tab === "processes" && (
              <ProcessList
                config={config}
                onChange={(patch) => patchConfig(patch, true)}
                onSave={() => void save()}
                saving={saving}
              />
            )}
            {tab === "monitor" && (
              <MonitorConfig
                config={config}
                onChange={patchConfig}
                onSave={(override) => void save(override)}
                saving={saving}
              />
            )}
            {tab === "settings" && <SettingsTab />}
          </div>
        </div>
      </main>
    </div>
  );
}
