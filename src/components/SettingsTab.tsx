import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useTheme } from "../ThemeContext";

interface AppSettings {
  theme: string;
  autostart: boolean;
  start_minimized: boolean;
  close_to_tray: boolean;
  check_updates: boolean;
}

const DEFAULT: AppSettings = {
  theme: "system",
  autostart: false,
  start_minimized: false,
  close_to_tray: true,
  check_updates: true,
};

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex items-center w-11 h-6 rounded-full transition-colors flex-shrink-0 ${
        checked ? "bg-red-500" : "bg-slate-300 dark:bg-slate-600"
      }`}
    >
      <span
        className={`inline-block w-4 h-4 bg-white rounded-full shadow transition-transform ${
          checked ? "translate-x-6" : "translate-x-1"
        }`}
      />
    </button>
  );
}

function SettingRow({
  label,
  description,
  children,
}: {
  label: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 py-3.5 border-b border-black/5 dark:border-white/5 last:border-0">
      <div className="min-w-0">
        <div className="text-sm font-medium text-slate-800 dark:text-slate-200">{label}</div>
        {description && (
          <div className="text-xs text-slate-400 dark:text-slate-500 mt-0.5">{description}</div>
        )}
      </div>
      <div className="flex-shrink-0">{children}</div>
    </div>
  );
}

type UpdateStatus = "idle" | "checking" | "up-to-date" | "available" | "error";

interface UpdateInfo {
  tag: string;
  changelog: string;
}

function renderChangelog(text: string): React.ReactNode {
  const lines = text.split("\n");
  return (
    <div className="space-y-0.5">
      {lines.map((line, i) => {
        if (line.startsWith("## "))
          return (
            <p key={i} className="font-semibold text-sm text-slate-800 dark:text-slate-200 mt-3 first:mt-0 mb-1">
              {line.slice(3)}
            </p>
          );
        if (line.startsWith("### "))
          return (
            <p key={i} className="text-xs font-semibold uppercase tracking-wider text-slate-400 dark:text-slate-500 mt-2">
              {line.slice(4)}
            </p>
          );
        if (line.startsWith("- ") || line.startsWith("* "))
          return (
            <div key={i} className="flex gap-2 text-sm text-slate-700 dark:text-slate-300">
              <span className="text-slate-400 shrink-0 mt-0.5">•</span>
              <span>{line.slice(2)}</span>
            </div>
          );
        if (line.trim() === "") return <div key={i} className="h-1.5" />;
        return (
          <p key={i} className="text-sm text-slate-600 dark:text-slate-400">
            {line}
          </p>
        );
      })}
    </div>
  );
}

function UpdateDialog({
  info,
  onClose,
  onUpdate,
  installing,
  progress,
}: {
  info: UpdateInfo;
  onClose: () => void;
  onUpdate: () => void;
  installing: boolean;
  progress: number | null;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={!installing ? onClose : undefined} />
      <div className="relative bg-white dark:bg-[#1c1c1c] rounded-2xl shadow-2xl w-full max-w-md overflow-hidden border border-black/8 dark:border-white/8">
        <div className="px-5 py-4 border-b border-black/8 dark:border-white/8">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <span className="w-2 h-2 rounded-full bg-amber-400 shrink-0" />
              <div className="text-sm font-semibold text-slate-900 dark:text-slate-100">
                Update verfügbar – v{info.tag}
              </div>
            </div>
            {!installing && (
              <button
                onClick={onClose}
                className="text-slate-400 hover:text-slate-600 dark:hover:text-slate-200 transition-colors"
              >
                <svg xmlns="http://www.w3.org/2000/svg" className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            )}
          </div>
          <div className="text-xs text-slate-400 dark:text-slate-500 mt-0.5 ml-4">
            Änderungen in dieser Version
          </div>
        </div>

        <div className="px-5 py-4 max-h-64 overflow-y-auto">
          {info.changelog
            ? renderChangelog(info.changelog)
            : <p className="text-sm text-slate-400">Keine Changelogs vorhanden.</p>
          }
        </div>

        {installing && (
          <div className="px-5 pb-2">
            <div className="w-full h-1.5 bg-slate-200 dark:bg-slate-700 rounded-full overflow-hidden">
              <div
                className="h-full bg-red-500 rounded-full transition-all duration-200"
                style={{ width: progress !== null ? `${progress}%` : "100%" }}
              />
            </div>
            <p className="text-xs text-slate-400 mt-1.5">
              {progress !== null ? `${progress}% heruntergeladen…` : "Wird installiert…"}
            </p>
          </div>
        )}

        <div className="px-5 py-4 border-t border-black/8 dark:border-white/8 flex justify-end">
          <button
            onClick={onUpdate}
            disabled={installing}
            className="px-4 py-2 rounded-lg text-sm font-medium bg-red-500 hover:bg-red-600 disabled:opacity-50 disabled:cursor-not-allowed text-white transition-colors"
          >
            {installing ? "Wird aktualisiert…" : "Jetzt aktualisieren"}
          </button>
        </div>
      </div>
    </div>
  );
}

export function SettingsTab() {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT);
  const [version, setVersion] = useState("");
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>("idle");
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [pendingUpdate, setPendingUpdate] = useState<Update | null>(null);
  const [showDialog, setShowDialog] = useState(false);
  const [isInstalling, setIsInstalling] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState<number | null>(null);
  const { setTheme } = useTheme();
  const autoCheckDone = useRef(false);

  useEffect(() => {
    invoke<AppSettings>("load_settings").then(setSettings).catch(() => {});
    getVersion().then(setVersion).catch(() => {});
  }, []);

  useEffect(() => {
    if (!autoCheckDone.current && version && settings.check_updates) {
      autoCheckDone.current = true;
      checkUpdates(true);
    }
  }, [version, settings.check_updates]);

  async function patch(partial: Partial<AppSettings>) {
    const next = { ...settings, ...partial };
    setSettings(next);
    try {
      await invoke("save_settings", { s: next });
      if (partial.theme) {
        setTheme(partial.theme as "light" | "dark" | "system");
        await invoke("set_window_theme", { theme: partial.theme });
      }
    } catch (e) {
      console.error(e);
    }
  }

  async function checkUpdates(autoCheck = false) {
    setUpdateStatus("checking");
    try {
      const update = await check();
      if (!update) {
        setUpdateStatus("up-to-date");
        return;
      }
      const info: UpdateInfo = { tag: update.version, changelog: (update.body ?? "").trim() };
      setUpdateInfo(info);
      setPendingUpdate(update);
      setUpdateStatus("available");
      if (!autoCheck) {
        setShowDialog(true);
      }
      invoke("show_update_notification", {
        title: "Update verfügbar",
        body: `Intelligent Hz Changer v${update.version} ist verfügbar.`,
      }).catch(() => {});
    } catch {
      setUpdateStatus("error");
    }
  }

  async function handleUpdate() {
    if (!pendingUpdate) return;
    setIsInstalling(true);
    setDownloadProgress(null);
    try {
      let downloaded = 0;
      let total = 0;
      await pendingUpdate.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setDownloadProgress(total > 0 ? Math.round((downloaded / total) * 100) : null);
        }
      });
      await relaunch();
    } catch (e) {
      console.error(e);
      setIsInstalling(false);
    }
  }

  const themeOptions = [
    { value: "system", label: "System" },
    { value: "light", label: "Hell" },
    { value: "dark", label: "Dunkel" },
  ];

  return (
    <>
      {showDialog && updateInfo && (
        <UpdateDialog
          info={updateInfo}
          onClose={() => setShowDialog(false)}
          onUpdate={handleUpdate}
          installing={isInstalling}
          progress={downloadProgress}
        />
      )}

      <div className="space-y-6">
        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wider text-slate-400 dark:text-slate-500 mb-1">
            Darstellung
          </h2>
          <div className="bg-slate-50 dark:bg-[#242424] rounded-2xl border border-black/8 dark:border-white/8 px-4">
            <SettingRow label="Theme" description="Farbschema der App">
              <div className="flex gap-1 bg-black/5 dark:bg-white/5 rounded-lg p-0.5">
                {themeOptions.map((opt) => (
                  <button
                    key={opt.value}
                    onClick={() => patch({ theme: opt.value })}
                    className={`px-3 py-1 rounded-md text-xs font-medium transition-all ${
                      settings.theme === opt.value
                        ? "bg-white dark:bg-[#1c1c1c] text-slate-900 dark:text-slate-100 shadow-sm"
                        : "text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
                    }`}
                  >
                    {opt.label}
                  </button>
                ))}
              </div>
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wider text-slate-400 dark:text-slate-500 mb-1">
            Startverhalten
          </h2>
          <div className="bg-slate-50 dark:bg-[#242424] rounded-2xl border border-black/8 dark:border-white/8 px-4">
            <SettingRow label="Autostart" description="App beim Windows-Start automatisch starten">
              <Toggle checked={settings.autostart} onChange={(v) => patch({ autostart: v })} />
            </SettingRow>
            <SettingRow label="Minimiert starten" description="App beim Start direkt im Tray verstecken">
              <Toggle
                checked={settings.start_minimized}
                onChange={(v) => patch({ start_minimized: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wider text-slate-400 dark:text-slate-500 mb-1">
            Systemtray
          </h2>
          <div className="bg-slate-50 dark:bg-[#242424] rounded-2xl border border-black/8 dark:border-white/8 px-4">
            <SettingRow
              label="In Tray minimieren beim Schließen"
              description="Fenster schließen versteckt die App statt sie zu beenden"
            >
              <Toggle
                checked={settings.close_to_tray}
                onChange={(v) => patch({ close_to_tray: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wider text-slate-400 dark:text-slate-500 mb-1">
            Updates
          </h2>
          <div className="bg-slate-50 dark:bg-[#242424] rounded-2xl border border-black/8 dark:border-white/8 px-4">
            <SettingRow
              label="Automatisch nach Updates suchen"
              description="Beim Start auf neue Versionen prüfen"
            >
              <Toggle
                checked={settings.check_updates}
                onChange={(v) => patch({ check_updates: v })}
              />
            </SettingRow>
            <SettingRow
              label="Jetzt nach Updates suchen"
              description={version ? `Aktuelle Version: v${version}` : undefined}
            >
              <div className="flex items-center gap-2">
                {updateStatus === "up-to-date" && (
                  <span className="text-xs text-emerald-500 font-medium">Aktuell</span>
                )}
                {updateStatus === "available" && (
                  <button
                    onClick={() => setShowDialog(true)}
                    className="text-xs text-red-500 font-medium hover:text-red-600 transition-colors underline underline-offset-2"
                  >
                    v{updateInfo?.tag} verfügbar
                  </button>
                )}
                {updateStatus === "error" && (
                  <span className="text-xs text-red-400 font-medium">Fehler</span>
                )}
                <button
                  onClick={() => checkUpdates(false)}
                  disabled={updateStatus === "checking"}
                  className="px-3 py-1.5 rounded-lg text-xs font-medium bg-slate-200 dark:bg-slate-700 text-slate-700 dark:text-slate-200 hover:bg-slate-300 dark:hover:bg-slate-600 disabled:opacity-50 transition-colors"
                >
                  {updateStatus === "checking" ? "Wird geprüft…" : "Prüfen"}
                </button>
              </div>
            </SettingRow>
          </div>
        </section>
      </div>
    </>
  );
}
