import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Settings } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MonitorConfig } from "./components/MonitorConfig";
import { ProcessList } from "./components/ProcessList";
import { SettingsTab } from "./components/SettingsTab";
import { StatusView } from "./components/StatusView";
import { Switch } from "./components/Switch";
import { statusMonitor, type WatchConfig } from "./types";
import { clearHzError, useHzStatus } from "./useHzStatus";

type Tab = "status" | "processes" | "monitor" | "settings";

const DEFAULT_CONFIG: WatchConfig = {
  watched_processes: [],
  monitors: {},
  disabled_processes: [],
  process_overrides: {},
};

function TabIcon({ children }: { children: React.ReactNode }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
}

const PULSE_PATH = "M22 12h-4l-3 9L9 3l-3 9H2";

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
      icon: (
        <TabIcon>
          <path d={PULSE_PATH} />
        </TabIcon>
      ),
    },
    {
      id: "processes",
      label: t("tabs.processes"),
      icon: (
        <TabIcon>
          <path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01" />
        </TabIcon>
      ),
    },
    {
      id: "monitor",
      label: t("tabs.monitor"),
      icon: (
        <TabIcon>
          <rect x="2" y="3" width="20" height="14" rx="2" />
          <path d="M8 21h8M12 17v4" />
        </TabIcon>
      ),
    },
    {
      id: "settings",
      label: t("tabs.settings"),
      icon: <Settings size={14} />,
    },
  ];

  return (
    <div className="h-screen bg-canvas flex flex-col overflow-hidden">
      {/* Header */}
      <header className="flex items-center gap-3 px-5 pt-4 pb-3.5 shrink-0">
        <div className="w-9 h-9 rounded-[10px] bg-accent flex items-center justify-center shrink-0 shadow-[inset_0_1px_rgba(255,255,255,0.25)]">
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="#fff"
            strokeWidth="2.25"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d={PULSE_PATH} />
          </svg>
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-[15px] font-semibold tracking-[-0.01em]">
            Intelligent Hz Changer
          </div>
          <div className="text-xs text-muted mt-px">{t("app.subtitle")}</div>
        </div>
        <span className="font-medium text-fg-2">{t("app.active")}</span>
        <Switch
          large
          checked={active}
          title={t("app.active")}
          onChange={(v) => {
            setActive(v);
            invoke("set_enabled", { value: v }).catch(console.error);
          }}
        />
      </header>

      {/* Tab bar */}
      <nav className="flex flex-wrap items-center gap-1 px-5 pb-3 border-b border-line-subtle shrink-0">
        {tabs.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            aria-current={tab === t.id ? "page" : undefined}
            className={`flex items-center gap-2 h-7.5 px-2.75 rounded-lg border font-medium btn-press ${
              tab === t.id
                ? "hl bg-tab-active border-line-strong text-fg [&>svg]:text-accent-text"
                : "border-transparent text-fg-3 hover:text-fg"
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
        <div className="flex-1" />
        {/* Hz status pill */}
        <div className="hl flex items-center gap-2 h-7 px-2.5 rounded-lg border border-line-control bg-surface">
          <span
            className={`w-1.5 h-1.5 rounded-full shrink-0 ${gameMode ? "bg-accent dot-pulse" : "bg-faint"}`}
          />
          <span
            key={String(gameMode)}
            className="text-xs text-fg-3 whitespace-nowrap badge-anim"
          >
            {t(gameMode ? "mode.game" : "mode.standard")}
          </span>
          {headerHz != null && (
            <span
              key={headerHz}
              className="font-mono text-xs font-semibold badge-anim"
            >
              {headerHz} Hz
            </span>
          )}
        </div>
      </nav>

      {/* Content */}
      <main className="flex-1 overflow-y-auto">
        {/* A refused Hz switch leaves the monitor on the wrong rate, which is
            invisible otherwise — so it is shown on every tab, not just the
            monitor one. */}
        {lastError && (
          <div className="mx-5 mt-5 flex items-start gap-3 rounded-xl border border-accent-line bg-accent-soft px-4 py-3">
            <div className="flex-1 min-w-0">
              <div className="font-semibold text-accent-text">
                {t("app.hzErrorTitle", { hz: lastError.target_hz })}
              </div>
              <div className="text-xs text-accent-text/90 mt-0.5 wrap-break-word select-text">
                {lastError.error}
              </div>
            </div>
            <button
              onClick={clearHzError}
              aria-label={t("app.dismiss")}
              className="shrink-0 text-accent-text hover:opacity-70 text-lg leading-none px-1"
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
