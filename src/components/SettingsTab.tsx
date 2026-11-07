import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import i18n from "../i18n";
import { useTheme } from "../useTheme";
import { SegmentedControl } from "./SegmentedControl";
import { Switch } from "./Switch";

interface AppSettings {
  theme: string;
  autostart: boolean;
  start_minimized: boolean;
  close_to_tray: boolean;
  check_updates: boolean;
  language: string;
  debug_logging: boolean;
}

const DEFAULT: AppSettings = {
  theme: "system",
  autostart: false,
  start_minimized: false,
  close_to_tray: true,
  check_updates: true,
  language: "de",
  debug_logging: false,
};

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
    <div className="flex items-center justify-between gap-4 py-3.5 border-b border-line-subtle last:border-0">
      <div className="min-w-0">
        <div className="font-semibold">
          {label}
        </div>
        {description && (
          <div className="text-xs text-muted mt-0.5">
            {description}
          </div>
        )}
      </div>
      <div className="shrink-0">{children}</div>
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
            <p
              key={i}
              className="font-semibold text-sm text-fg mt-3 first:mt-0 mb-1"
            >
              {line.slice(3)}
            </p>
          );
        if (line.startsWith("### "))
          return (
            <p
              key={i}
              className="text-xs font-semibold uppercase tracking-wider text-muted mt-2"
            >
              {line.slice(4)}
            </p>
          );
        if (line.startsWith("- ") || line.startsWith("* "))
          return (
            <div
              key={i}
              className="flex gap-2 text-sm text-fg-2"
            >
              <span className="text-muted shrink-0 mt-0.5">•</span>
              <span>{line.slice(2)}</span>
            </div>
          );
        if (line.trim() === "") return <div key={i} className="h-1.5" />;
        return (
          <p key={i} className="text-sm text-fg-3">
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
  const { t } = useTranslation();
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <div
        className="absolute inset-0 bg-black/50 backdrop-blur-sm"
        onClick={!installing ? onClose : undefined}
      />
      <div className="relative bg-surface rounded-xl shadow-2xl w-full max-w-md overflow-hidden border border-line-strong">
        <div className="px-5 py-4 border-b border-line-strong">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <span className="w-2 h-2 rounded-full bg-amber-400 shrink-0" />
              <div className="text-sm font-semibold text-fg">
                {t("settings.updateAvailable", { version: info.tag })}
              </div>
            </div>
            {!installing && (
              <button
                onClick={onClose}
                className="text-muted hover:text-fg transition-colors"
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  className="w-4 h-4"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            )}
          </div>
          <div className="text-xs text-muted mt-0.5 ml-4">
            {t("settings.changelog")}
          </div>
        </div>

        <div className="px-5 py-4 max-h-64 overflow-y-auto">
          {info.changelog ? (
            renderChangelog(info.changelog)
          ) : (
            <p className="text-sm text-muted">
              {t("settings.noChangelog")}
            </p>
          )}
        </div>

        {installing && (
          <div className="px-5 pb-2">
            <div className="w-full h-1.5 bg-chip rounded-full overflow-hidden">
              <div
                className="h-full bg-accent rounded-full transition-all duration-200"
                style={{ width: progress !== null ? `${progress}%` : "100%" }}
              />
            </div>
            <p className="text-xs text-muted mt-1.5">
              {progress !== null
                ? t("settings.downloadProgress", { pct: progress })
                : t("settings.installing")}
            </p>
          </div>
        )}

        <div className="px-5 py-4 border-t border-line-strong flex justify-end">
          <button
            onClick={onUpdate}
            disabled={installing}
            className="btn-accent btn-press h-9 px-4"
          >
            {installing ? t("settings.updating") : t("settings.updateNow")}
          </button>
        </div>
      </div>
    </div>
  );
}

export function SettingsTab() {
  const { t } = useTranslation();
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
    invoke<AppSettings>("load_settings")
      .then((s) => {
        setSettings(s);
        if (s.language) void i18n.changeLanguage(s.language);
      })
      .catch(() => undefined);
    getVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, []);

  const checkUpdates = useCallback(
    async (autoCheck = false) => {
      setUpdateStatus("checking");
      try {
        const update = await check();
        if (!update) {
          setUpdateStatus("up-to-date");
          return;
        }
        const info: UpdateInfo = {
          tag: update.version,
          changelog: (update.body ?? "").trim(),
        };
        setUpdateInfo(info);
        setPendingUpdate(update);
        setUpdateStatus("available");
        if (!autoCheck) {
          setShowDialog(true);
        }
        invoke("show_update_notification", {
          title: t("settings.notifUpdateTitle"),
          body: t("settings.notifUpdateBody", { version: update.version }),
        }).catch(() => undefined);
      } catch {
        setUpdateStatus("error");
      }
    },
    [t],
  );

  useEffect(() => {
    if (!autoCheckDone.current && version && settings.check_updates) {
      autoCheckDone.current = true;
      void checkUpdates(true);
    }
  }, [version, settings.check_updates, checkUpdates]);

  async function patch(partial: Partial<AppSettings>) {
    const next = { ...settings, ...partial };
    setSettings(next);
    try {
      await invoke("save_settings", { s: next });
      if (partial.theme) {
        setTheme(partial.theme as "light" | "dark" | "system");
        await invoke("set_window_theme", { theme: partial.theme });
      }
      if (partial.language) {
        await i18n.changeLanguage(partial.language);
      }
    } catch (e) {
      console.error(e);
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
          setDownloadProgress(
            total > 0 ? Math.round((downloaded / total) * 100) : null,
          );
        }
      });
      await relaunch();
    } catch (e) {
      console.error(e);
      setIsInstalling(false);
    }
  }

  const themeOptions = [
    { value: "system", label: t("settings.themeSystem") },
    { value: "light", label: t("settings.themeLight") },
    { value: "dark", label: t("settings.themeDark") },
  ];

  const languageOptions = [
    { value: "de", label: t("settings.langDE") },
    { value: "en", label: t("settings.langEN") },
  ];

  return (
    <>
      {showDialog &&
        updateInfo &&
        createPortal(
          <UpdateDialog
            info={updateInfo}
            onClose={() => setShowDialog(false)}
            onUpdate={() => void handleUpdate()}
            installing={isInstalling}
            progress={downloadProgress}
          />,
          document.body,
        )}

      <div className="space-y-6">
        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionAppearance")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.themeLabel")}
              description={t("settings.themeDesc")}
            >
              <SegmentedControl
                options={themeOptions}
                value={settings.theme}
                onChange={(v) => void patch({ theme: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionLanguage")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.languageLabel")}
              description={t("settings.languageDesc")}
            >
              <SegmentedControl
                options={languageOptions}
                value={settings.language ?? i18n.language}
                onChange={(v) => void patch({ language: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionStartup")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.autostartLabel")}
              description={t("settings.autostartDesc")}
            >
              <Switch
                checked={settings.autostart}
                onChange={(v) => void patch({ autostart: v })}
              />
            </SettingRow>
            <SettingRow
              label={t("settings.startMinimizedLabel")}
              description={t("settings.startMinimizedDesc")}
            >
              <Switch
                checked={settings.start_minimized}
                onChange={(v) => void patch({ start_minimized: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionTray")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.closeToTrayLabel")}
              description={t("settings.closeToTrayDesc")}
            >
              <Switch
                checked={settings.close_to_tray}
                onChange={(v) => void patch({ close_to_tray: v })}
              />
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionUpdates")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.checkUpdatesLabel")}
              description={t("settings.checkUpdatesDesc")}
            >
              <Switch
                checked={settings.check_updates}
                onChange={(v) => void patch({ check_updates: v })}
              />
            </SettingRow>
            <SettingRow
              label={t("settings.checkNowLabel")}
              description={
                version ? t("settings.currentVersion", { version }) : undefined
              }
            >
              <div className="flex items-center gap-2">
                {updateStatus === "up-to-date" && (
                  <span className="text-xs text-emerald-500 font-medium">
                    {t("settings.upToDate")}
                  </span>
                )}
                {updateStatus === "available" && (
                  <button
                    onClick={() => setShowDialog(true)}
                    className="text-xs text-accent-text font-medium hover:opacity-80 transition-opacity underline underline-offset-2"
                  >
                    {t("settings.versionAvailable", {
                      version: updateInfo?.tag,
                    })}
                  </button>
                )}
                {updateStatus === "error" && (
                  <span className="text-xs text-accent-text font-medium">
                    {t("settings.updateError")}
                  </span>
                )}
                <button
                  onClick={() => void checkUpdates(false)}
                  disabled={updateStatus === "checking"}
                  className="btn hl btn-press"
                >
                  {updateStatus === "checking"
                    ? t("settings.checking")
                    : t("settings.checkBtn")}
                </button>
              </div>
            </SettingRow>
          </div>
        </section>

        <section>
          <h2 className="text-sm font-semibold mb-2.5">
            {t("settings.sectionDiagnostics")}
          </h2>
          <div className="card px-4">
            <SettingRow
              label={t("settings.debugLoggingLabel")}
              description={t("settings.debugLoggingDesc")}
            >
              <Switch
                checked={settings.debug_logging}
                onChange={(v) => void patch({ debug_logging: v })}
              />
            </SettingRow>
            <SettingRow
              label={t("settings.openLogLabel")}
              description={t("settings.openLogDesc")}
            >
              <button
                onClick={() =>
                  void invoke("open_log_file").catch((e) => console.error(e))
                }
                className="btn hl btn-press"
              >
                {t("settings.openLogBtn")}
              </button>
            </SettingRow>
          </div>
        </section>
      </div>
    </>
  );
}
