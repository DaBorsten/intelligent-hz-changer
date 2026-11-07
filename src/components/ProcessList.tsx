import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  type KeyboardEvent,
  type PointerEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { isWindows, looksLikeExecutablePath, platformKey } from "../platform";
import { visibleInterval } from "../visibleInterval";
import { buildDisplayNums, fallbackGameHz, getMonitorLabel } from "../monitors";
import type {
  HzOverride,
  MonitorInfoExtended,
  WatchConfig,
  WatchedProcess,
} from "../types";
import { wpKey, wpName } from "../types";
import { CustomSelect } from "./CustomSelect";
import { Switch } from "./Switch";

interface Props {
  config: WatchConfig;
  /** Applies and saves a change to the watch list or its per-game settings. */
  onChange: (patch: Partial<WatchConfig>) => void;
  onSave: () => void;
  saving: boolean;
}

interface RunningProcess {
  name: string;
  path: string | null;
}

const ICON_CACHE_KEY = "hz-process-icons";
const ICON_CACHE_MAX = 100;

function filenameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() ?? "";
}

function loadIconCache(): Record<string, string | null> {
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
  const keys = Object.keys(cache);
  if (keys.length > ICON_CACHE_MAX) {
    for (const stale of keys.slice(0, keys.length - ICON_CACHE_MAX)) {
      delete cache[stale];
    }
  }
  try {
    localStorage.setItem(ICON_CACHE_KEY, JSON.stringify(cache));
  } catch {
    // Cache writes are best-effort; private mode or quota limits can fail here.
  }
}

/** Select value for "use the monitor's global setting". */
const GLOBAL = "global";
const KEEP = "keep";

function overrideToValue(o: HzOverride | undefined): string {
  if (o === undefined) return GLOBAL;
  return o === "keep" ? KEEP : String(o.hz);
}

function valueToOverride(v: string): HzOverride | undefined {
  if (v === GLOBAL) return undefined;
  return v === KEEP ? "keep" : { hz: Number(v) };
}

/** What a row's badge says about its per-monitor settings, if anything. */
function overrideBadge(
  overrides: Record<string, HzOverride> | undefined,
  custom: string,
): string | null {
  const values = Object.values(overrides ?? {});
  if (values.length === 0) return null;
  const hz = new Set(values.map((o) => (o === "keep" ? -1 : o.hz)));
  const [only] = hz;
  return hz.size === 1 && only > 0 ? `${only} Hz` : custom;
}

function FolderIcon({ size }: { size: number }) {
  return (
    <svg
      className="shrink-0"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinejoin="round"
    >
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    </svg>
  );
}

export function ProcessList({ config, onChange }: Props) {
  const processes = config.watched_processes;
  const disabled = config.disabled_processes;
  const overrides = config.process_overrides;
  const { t } = useTranslation();
  const pickerBackdropPointerStartedOutside = useRef(false);
  const editBackdropPointerStartedOutside = useRef(false);
  const [input, setInput] = useState("");
  const [runningKeys, setRunningKeys] = useState<string[]>([]);
  const [processCounts, setProcessCounts] = useState<Record<string, number>>(
    {},
  );
  const [lastSeen, setLastSeen] = useState<Record<string, number>>({});
  const [now, setNow] = useState(() => Date.now());
  const [processIcons, setProcessIcons] = useState<
    Record<string, string | null>
  >(() => loadIconCache());
  const [removingProcesses, setRemovingProcesses] = useState<Set<string>>(
    new Set(),
  );
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerList, setPickerList] = useState<RunningProcess[]>([]);
  const [pickerSearch, setPickerSearch] = useState("");
  const [pickerLoading, setPickerLoading] = useState(false);
  const [pickerIcons, setPickerIcons] = useState<
    Record<string, string | null | undefined>
  >({});
  const [addError, setAddError] = useState("");

  const [editDialog, setEditDialog] = useState<WatchedProcess | null>(null);
  const [editName, setEditName] = useState("");
  const [editPath, setEditPath] = useState("");
  const [editPathError, setEditPathError] = useState("");
  const [editOverrides, setEditOverrides] = useState<
    Record<string, HzOverride>
  >({});
  // Loaded when the edit dialog first opens; enumerating is too slow for every row.
  const [monitors, setMonitors] = useState<MonitorInfoExtended[] | null>(null);
  const [supportedHz, setSupportedHz] = useState<Record<string, number[]>>({});

  const updateRunningKeys = useCallback((keys: string[]) => {
    const timestamp = Date.now();
    setNow(timestamp);
    setRunningKeys(keys);
    if (keys.length === 0) return;
    setLastSeen((prev) => {
      const next = { ...prev };
      for (const k of keys) next[k] = timestamp;
      return next;
    });
  }, []);

  useEffect(() => {
    const refresh = () => {
      invoke<string[]>("get_running_watched")
        .then(updateRunningKeys)
        .catch(() => undefined);
      invoke<Record<string, number>>("get_process_counts")
        .then(setProcessCounts)
        .catch(() => undefined);
    };
    refresh();
    return visibleInterval(refresh, 3000);
  }, [updateRunningKeys]);

  useEffect(() => {
    const cache = loadIconCache();
    for (const wp of processes) {
      const name = wpName(wp).toLowerCase();
      if (processIcons[name] || cache[name]) continue;
      invoke<string | null>("get_process_icon", {
        processName: wpName(wp),
        exePath: typeof wp === "object" ? wp.path : undefined,
      })
        .then((icon) => {
          if (icon) {
            saveIconToCache(name, icon);
            setProcessIcons((prev) => ({ ...prev, [name]: icon }));
          }
        })
        .catch(() => undefined);
    }
  }, [processes, processIcons]);

  function openPicker() {
    setPickerOpen(true);
    setPickerSearch("");
    setPickerLoading(true);
    invoke<RunningProcess[]>("get_running_processes_with_paths")
      .then((list) => {
        setPickerList(list);
        const cache = loadIconCache();
        for (const rp of list) {
          const key = rp.name.toLowerCase();
          if (pickerIcons[key] || cache[key]) continue;
          invoke<string | null>("get_process_icon", {
            processName: rp.name,
            exePath: rp.path ?? undefined,
          })
            .then((icon) => {
              if (icon) {
                saveIconToCache(key, icon);
                setPickerIcons((prev) => ({ ...prev, [key]: icon }));
              }
            })
            .catch(() => undefined);
        }
      })
      .catch(() => setPickerList([]))
      .finally(() => setPickerLoading(false));
  }

  // ponytail: always use path when available — does NOT close picker
  function pickProcess(rp: RunningProcess) {
    const entry: WatchedProcess = rp.path
      ? { name: rp.name, path: rp.path }
      : rp.name;
    const key = wpKey(entry);
    if (!processes.some((p) => wpKey(p) === key)) {
      onChange({ watched_processes: [...processes, entry] });
    }
  }

  async function browseExe(onPick: (path: string, filename: string) => void) {
    const selected = await openDialog({
      // Linux executables have no extension to filter on.
      filters: isWindows
        ? [{ name: t("processes.browseExeFilter"), extensions: ["exe"] }]
        : undefined,
      multiple: false,
      title: t(platformKey("processes.browseExeTitle")),
    });
    if (!selected || typeof selected !== "string") return;
    onPick(selected, filenameFromPath(selected));
  }

  async function add() {
    const raw = input.trim();
    if (!raw) return;
    setAddError("");
    // ponytail: path if contains separator, else just name
    const isPath = raw.includes("/") || (isWindows && raw.includes("\\"));
    let normalized = raw;
    // Only Windows names an executable by its extension.
    if (isWindows && !normalized.toLowerCase().endsWith(".exe")) {
      normalized += ".exe";
    }
    const name = isPath
      ? (normalized.replace(/\\/g, "/").split("/").pop() ?? normalized)
      : normalized;
    const entry: WatchedProcess = isPath ? { name, path: normalized } : name;
    if (isPath) {
      const exists = await invoke<boolean>("check_exe_exists", {
        path: normalized,
      });
      if (!exists) {
        setAddError(t(platformKey("processes.notFound")));
        return;
      }
    }
    const key = wpKey(entry);
    if (processes.some((p) => wpKey(p) === key)) return;
    onChange({ watched_processes: [...processes, entry] });
    setInput("");
  }

  function remove(wp: WatchedProcess) {
    const key = wpKey(wp);
    setRemovingProcesses((prev) => new Set(prev).add(key));
    setTimeout(() => {
      const { [key]: _removed, ...rest } = overrides;
      onChange({
        watched_processes: processes.filter((p) => wpKey(p) !== key),
        disabled_processes: disabled.filter((d) => d !== key),
        process_overrides: rest,
      });
      setRemovingProcesses((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }, 220);
  }

  function setEnabled(wp: WatchedProcess, enabled: boolean) {
    const key = wpKey(wp);
    const rest = disabled.filter((d) => d !== key);
    onChange({ disabled_processes: enabled ? rest : [...rest, key] });
  }

  function loadMonitors() {
    if (monitors) return;
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
      })
      .catch(() => setMonitors([]));
  }

  function openEditDialog(wp: WatchedProcess) {
    setEditDialog(wp);
    setEditName(wpName(wp));
    setEditPath(typeof wp === "string" ? "" : wp.path);
    setEditPathError("");
    setEditOverrides(overrides[wpKey(wp)] ?? {});
    loadMonitors();
  }

  function setEditOverride(monitor: string, value: string) {
    setEditOverrides((prev) => {
      const { [monitor]: _old, ...rest } = prev;
      const o = valueToOverride(value);
      return o === undefined ? rest : { ...rest, [monitor]: o };
    });
  }

  function closeEditDialog() {
    setEditDialog(null);
  }

  function handleEditPathChange(path: string) {
    setEditPath(path);
    setEditPathError("");
    if (looksLikeExecutablePath(path)) {
      const filename = filenameFromPath(path.trim());
      if (filename) setEditName(filename);
    }
  }

  async function commitEditDialog() {
    if (!editDialog) return;
    const name = editName.trim();
    if (!name) return;
    const path = editPath.trim();
    if (looksLikeExecutablePath(path)) {
      const exists = await invoke<boolean>("check_exe_exists", { path });
      if (!exists) {
        setEditPathError(t(platformKey("processes.notFound")));
        return;
      }
    }
    const originalKey = wpKey(editDialog);
    const updated: WatchedProcess = path ? { name, path } : name;
    // The key follows name and path, so the pause state and per-monitor
    // settings move to the new key.
    const updatedKey = wpKey(updated);
    const { [originalKey]: _old, ...otherOverrides } = overrides;
    const nextOverrides =
      Object.keys(editOverrides).length > 0
        ? { ...otherOverrides, [updatedKey]: editOverrides }
        : otherOverrides;
    // An explicit Hz makes the backend manage that monitor, and it needs a
    // default to return to afterwards. Monitors never set up in the monitor
    // tab get one here: off globally, back to the rate they have now.
    const nextMonitors = { ...config.monitors };
    for (const [dev, o] of Object.entries(editOverrides)) {
      if (o === "keep" || nextMonitors[dev]) continue;
      const mon = monitors?.find((m) => m.device_name === dev);
      nextMonitors[dev] = {
        enabled: false,
        game_hz: fallbackGameHz(supportedHz[dev] ?? [], mon),
        default_hz: mon?.current_hz || 60,
      };
    }
    onChange({
      watched_processes: processes.map((p) =>
        wpKey(p) === originalKey ? updated : p,
      ),
      disabled_processes: disabled.map((d) =>
        d === originalKey ? updatedKey : d,
      ),
      process_overrides: nextOverrides,
      monitors: nextMonitors,
    });
    setEditDialog(null);
  }

  function handleKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") void add();
  }

  function handleBackdropPointerDown(
    e: PointerEvent<HTMLDivElement>,
    ref: { current: boolean },
  ) {
    ref.current = e.target === e.currentTarget;
  }

  function handleBackdropPointerUp(
    e: PointerEvent<HTMLDivElement>,
    ref: { current: boolean },
    onClose: () => void,
  ) {
    const shouldClose = ref.current && e.target === e.currentTarget;
    ref.current = false;
    if (shouldClose) onClose();
  }

  function formatLastSeen(wp: WatchedProcess): string {
    const key = wpKey(wp);
    const count = processCounts[key] ?? 0;
    const ts = lastSeen[key];
    const countStr = count > 0 ? t("processes.timesRun_one", { count }) : "";

    if (!ts) return countStr;

    const diffMs = now - ts;
    const diffMin = Math.floor(diffMs / 60000);
    const diffH = Math.floor(diffMin / 60);
    const diffD = Math.floor(diffH / 24);
    const ago =
      diffD >= 1
        ? t("processes.agoDays", { n: diffD })
        : diffH >= 1
          ? t("processes.agoHours", { n: diffH })
          : diffMin >= 1
            ? t("processes.agoMinutes", { n: diffMin })
            : t("processes.agoJustNow");

    return count > 0
      ? t("processes.lastSeenWithCount", { countStr, ago })
      : t("processes.lastSeenAgoOnly", { ago });
  }

  const filteredPicker = pickerList.filter((p) =>
    p.name.toLowerCase().includes(pickerSearch.toLowerCase()),
  );
  // A Windows process is always named after its exe, so picking a path fully
  // determines the name. Linux reports `comm`, which can differ from the file's
  // basename (shell scripts, Proton-wrapped games), so the name stays editable.
  const nameLocked = isWindows && looksLikeExecutablePath(editPath);

  return (
    <>
      <div className="space-y-4">
        {/* Add process card */}
        <div className="card p-4 flex flex-col gap-3 anim-fade-up stagger-1 relative z-10">
          <div className="flex items-center justify-between flex-wrap gap-3">
            <div className="flex items-baseline gap-2">
              <h2 className="font-semibold">{t("processes.addTitle")}</h2>
              <span className="text-xs text-muted">
                {t(platformKey("processes.addHint"))}
              </span>
            </div>
            <button onClick={openPicker} className="btn hl btn-press shrink-0">
              {t("processes.pickRunning")}
            </button>
          </div>

          {addError && <p className="text-xs text-accent-text">{addError}</p>}
          <div className="flex flex-wrap gap-2">
            <label className="field hl flex-[1_1_320px]">
              <FolderIcon size={15} />
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKey}
                placeholder={t(platformKey("processes.inputPlaceholder"))}
                className="flex-1 min-w-0 bg-transparent outline-none font-mono text-[12.5px] text-fg placeholder-faint"
              />
            </label>
            <button
              onClick={() => void browseExe((path) => setInput(path))}
              title={t(platformKey("processes.browseExeTitle"))}
              className="btn hl btn-press h-9 px-3.5 gap-2"
            >
              <FolderIcon size={14} />
              {t("processes.browse")}
            </button>
            <button
              onClick={() => void add()}
              disabled={!input.trim()}
              className="btn-accent btn-press h-9 px-3.5 gap-1.5"
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                strokeLinecap="round"
              >
                <path d="M12 5v14M5 12h14" />
              </svg>
              {t("processes.add")}
            </button>
          </div>
        </div>

        {/* Process list */}
        <div className="card overflow-hidden anim-fade-up stagger-2">
          <div className="flex items-center gap-2 px-4 pt-3.5 pb-2.5">
            <h2 className="font-semibold">{t("processes.watchedHeader")}</h2>
            <span className="font-mono text-[11px] text-fg-3 bg-chip border border-line-strong rounded-full px-1.75">
              {processes.length}
            </span>
          </div>
          {processes.length === 0 ? (
            <p className="text-muted italic px-4 py-3 border-t border-line-subtle">
              {t("processes.noProcesses")}
            </p>
          ) : (
            processes.map((wp, idx) => {
              const key = wpKey(wp);
              const name = wpName(wp);
              const path = typeof wp === "string" ? null : wp.path;
              const iconKey = name.toLowerCase();
              const icon = processIcons[iconKey];
              const isRunning = runningKeys.some((r) => r === key);
              const isEnabled = !disabled.includes(key);
              const badge = overrideBadge(
                overrides[key],
                t("processes.customHz"),
              );
              const lastSeenText = formatLastSeen(wp);

              return (
                <div
                  key={key}
                  className={`flex items-center gap-3.5 px-4 py-3 border-t border-line-subtle process-row ${
                    removingProcesses.has(key) ? "process-row-removing" : ""
                  }`}
                  style={{ animationDelay: `${Math.min(idx, 7) * 35}ms` }}
                >
                  <div
                    className={`flex flex-1 min-w-0 items-center gap-3.5 ${isEnabled ? "" : "opacity-45 grayscale"}`}
                    style={{
                      transition:
                        "opacity 200ms cubic-bezier(0.23,1,0.32,1), filter 200ms cubic-bezier(0.23,1,0.32,1)",
                    }}
                  >
                    <div
                      className={`w-8.5 h-8.5 rounded-lg flex items-center justify-center shrink-0 overflow-hidden font-semibold ${
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
                          className="w-8.5 h-8.5 object-contain"
                        />
                      ) : (
                        name.charAt(0).toUpperCase()
                      )}
                    </div>

                    <div className="flex-1 min-w-0">
                      <div className="font-mono font-medium truncate select-text">
                        {name}
                      </div>
                      {path && (
                        <div
                          className="font-mono text-[11px] text-faint mt-0.75 truncate select-text"
                          title={path}
                        >
                          {path}
                        </div>
                      )}
                      {lastSeenText && (
                        <div className="text-[11px] text-muted mt-0.75">
                          {lastSeenText}
                        </div>
                      )}
                    </div>
                  </div>

                  {badge && (
                    <span
                      className="text-[11px] font-medium bg-chip border border-line-strong text-fg-3 px-2 py-0.5 rounded-full shrink-0 tabular-nums"
                      title={t("processes.customHzTitle")}
                    >
                      {badge}
                    </span>
                  )}

                  {isRunning && (
                    <span className="text-[11px] font-semibold bg-accent-soft border border-accent-line text-accent-text px-2 py-0.5 rounded-full shrink-0">
                      {t("processes.active")}
                    </span>
                  )}

                  <Switch
                    checked={isEnabled}
                    onChange={(v) => setEnabled(wp, v)}
                    title={t(
                      isEnabled
                        ? "processes.pauseTitle"
                        : "processes.resumeTitle",
                    )}
                  />

                  <button
                    onClick={() => openEditDialog(wp)}
                    className="btn-icon btn-press"
                    title={t("processes.editTitle")}
                    aria-label={t("processes.editTitle")}
                  >
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
                      <path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" />
                    </svg>
                  </button>

                  <button
                    onClick={() => remove(wp)}
                    className="btn-icon btn-press"
                    title={t("processes.removeTitle")}
                    aria-label={t("processes.removeTitle")}
                  >
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                    >
                      <path d="M18 6 6 18M6 6l12 12" />
                    </svg>
                  </button>
                </div>
              );
            })
          )}
        </div>
      </div>

      {/* Process picker modal */}
      {pickerOpen &&
        createPortal(
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm"
            onPointerDown={(e) =>
              handleBackdropPointerDown(e, pickerBackdropPointerStartedOutside)
            }
            onPointerUp={(e) =>
              handleBackdropPointerUp(
                e,
                pickerBackdropPointerStartedOutside,
                () => setPickerOpen(false),
              )
            }
            onPointerCancel={() => {
              pickerBackdropPointerStartedOutside.current = false;
            }}
          >
            <div
              className="w-full max-w-sm mx-4 rounded-xl border border-line-strong bg-surface shadow-xl flex flex-col overflow-hidden"
              style={{ maxHeight: "min(520px, calc(100vh - 80px))" }}
            >
              <div className="flex items-start justify-between p-4 border-b border-line-subtle shrink-0">
                <div>
                  <h3 className="text-sm font-semibold text-fg">
                    {t("processes.pickerTitle")}
                  </h3>
                  <p className="text-xs text-muted mt-0.5">
                    {t("processes.pickerHint")}
                  </p>
                </div>
                <button
                  onClick={() => setPickerOpen(false)}
                  className="w-6 h-6 flex items-center justify-center text-muted hover:text-fg rounded transition-colors ml-2 shrink-0"
                >
                  <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                    <path
                      d="M2 2l8 8M10 2l-8 8"
                      stroke="currentColor"
                      strokeWidth="1.5"
                      strokeLinecap="round"
                    />
                  </svg>
                </button>
              </div>
              <div className="p-3 border-b border-line-subtle shrink-0">
                <div className="relative">
                  <svg
                    className="absolute left-2.5 top-1/2 -translate-y-1/2 pointer-events-none text-muted"
                    width="12"
                    height="12"
                    viewBox="0 0 12 12"
                    fill="none"
                  >
                    <circle
                      cx="5"
                      cy="5"
                      r="3.5"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <path
                      d="M8 8l2 2"
                      stroke="currentColor"
                      strokeWidth="1.3"
                      strokeLinecap="round"
                    />
                  </svg>
                  <input
                    autoFocus
                    type="text"
                    value={pickerSearch}
                    onChange={(e) => setPickerSearch(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") setPickerOpen(false);
                    }}
                    placeholder={t("processes.pickerSearch")}
                    className="w-full pl-8 pr-3 py-2 text-xs rounded-lg bg-well border border-line-strong text-fg placeholder-faint outline-none focus:border-accent-line transition-all"
                  />
                </div>
              </div>
              <div className="overflow-y-auto flex-1">
                {pickerLoading ? (
                  <p className="text-xs text-muted text-center py-6">
                    {t("processes.loading")}
                  </p>
                ) : filteredPicker.length === 0 ? (
                  <p className="text-xs text-muted text-center py-6">
                    {t("processes.noRunning")}
                  </p>
                ) : (
                  filteredPicker.map((rp) => {
                    const alreadyAdded = processes.some(
                      (p) => wpName(p).toLowerCase() === rp.name.toLowerCase(),
                    );
                    const iconKey = rp.name.toLowerCase();
                    const icon =
                      pickerIcons[iconKey] ?? loadIconCache()[iconKey] ?? null;
                    return (
                      <div
                        key={rp.name}
                        className="flex items-center gap-3 px-4 py-2.5 hover:bg-chip transition-colors"
                      >
                        <div
                          className={`w-8 h-8 flex items-center justify-center shrink-0 overflow-hidden ${icon ? "" : "rounded-lg bg-chip"}`}
                        >
                          {icon ? (
                            <img
                              src={icon}
                              alt=""
                              className="w-8 h-8 object-contain"
                            />
                          ) : (
                            <svg
                              width="16"
                              height="16"
                              viewBox="0 0 16 16"
                              fill="none"
                            >
                              <rect
                                x="1.5"
                                y="2.5"
                                width="13"
                                height="11"
                                rx="2"
                                stroke="currentColor"
                                strokeWidth="1.3"
                              />
                              <path
                                d="M1.5 6h13"
                                stroke="currentColor"
                                strokeWidth="1.1"
                              />
                              <circle cx="4" cy="4.25" r="0.8" fill="currentColor" />
                              <circle
                                cx="6.5"
                                cy="4.25"
                                r="0.8"
                                fill="currentColor"
                              />
                              <path
                                d="M4.5 9l2.5 1.5-2.5 1.5"
                                stroke="currentColor"
                                strokeWidth="1.1"
                                strokeLinecap="round"
                                strokeLinejoin="round"
                              />
                              <path
                                d="M9 11.5h2"
                                stroke="currentColor"
                                strokeWidth="1.1"
                                strokeLinecap="round"
                              />
                            </svg>
                          )}
                        </div>
                        <div className="flex-1 min-w-0">
                          <div
                            className={`text-xs font-semibold font-mono truncate ${alreadyAdded ? "text-muted" : "text-fg"}`}
                          >
                            {rp.name}
                          </div>
                          {rp.path && (
                            <div
                              className="text-[10px] font-mono text-muted truncate mt-0.5"
                              title={rp.path}
                            >
                              {rp.path}
                            </div>
                          )}
                        </div>
                        {alreadyAdded ? (
                          <span className="flex items-center gap-1 text-xs text-emerald-500 dark:text-emerald-400 shrink-0 font-medium">
                            <svg
                              width="12"
                              height="12"
                              viewBox="0 0 12 12"
                              fill="none"
                            >
                              <path
                                d="M2 6l3 3 5-5"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinecap="round"
                                strokeLinejoin="round"
                              />
                            </svg>
                            {t("processes.added")}
                          </span>
                        ) : (
                          <button
                            onClick={() => pickProcess(rp)}
                            className="flex items-center gap-1 text-xs text-muted hover:text-accent-text shrink-0 transition-colors font-medium btn-press"
                          >
                            <svg
                              width="11"
                              height="11"
                              viewBox="0 0 11 11"
                              fill="none"
                            >
                              <path
                                d="M5.5 1v9M1 5.5h9"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinecap="round"
                              />
                            </svg>
                            {t("processes.add")}
                          </button>
                        )}
                      </div>
                    );
                  })
                )}
              </div>
            </div>
          </div>,
          document.body,
        )}

      {/* Edit dialog modal */}
      {editDialog &&
        createPortal(
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm"
            onPointerDown={(e) =>
              handleBackdropPointerDown(e, editBackdropPointerStartedOutside)
            }
            onPointerUp={(e) =>
              handleBackdropPointerUp(
                e,
                editBackdropPointerStartedOutside,
                closeEditDialog,
              )
            }
            onPointerCancel={() => {
              editBackdropPointerStartedOutside.current = false;
            }}
          >
            <div className="w-full max-w-md mx-4 rounded-xl border border-line-strong bg-well shadow-xl p-5 space-y-4">
              <h3 className="text-sm font-semibold text-fg">
                {t("processes.editProcess")}
              </h3>

              <div className="space-y-1">
                <label className="text-xs text-muted">
                  {t("processes.processName")}
                </label>
                <input
                  autoFocus={!nameLocked}
                  type="text"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  readOnly={nameLocked}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void commitEditDialog();
                    if (e.key === "Escape") closeEditDialog();
                  }}
                  placeholder={t(platformKey("processes.namePlaceholder"))}
                  className={`w-full px-3 py-2 bg-surface border border-line-strong rounded-lg
                           text-sm font-mono text-fg outline-none
                           focus:border-accent-line transition-all ${
                             nameLocked ? "cursor-not-allowed opacity-75" : ""
                           }`}
                />
              </div>

              <div className="space-y-1">
                <label className="text-xs text-muted">
                  {t("processes.pathFilter")}
                </label>
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={editPath}
                    onChange={(e) => handleEditPathChange(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void commitEditDialog();
                      if (e.key === "Escape") closeEditDialog();
                    }}
                    placeholder={t(platformKey("processes.pathPlaceholder"))}
                    className={`flex-1 px-3 py-2 bg-surface border rounded-lg
                             text-xs font-mono text-fg-3 placeholder-faint outline-none
                             focus:ring-2 transition-all ${
                               editPathError
                                 ? "border-accent-line focus:border-accent-line"
                                 : "border-line-strong focus:border-accent-line"
                             }`}
                  />
                  <button
                    onClick={() =>
                      void browseExe((path, filename) => {
                        handleEditPathChange(path);
                        if (filename) setEditName(filename);
                      })
                    }
                    title={t(platformKey("processes.browseExeTitle"))}
                    className="px-3 py-2 rounded-lg border border-line-strong bg-surface text-muted hover:bg-chip transition-colors btn-press"
                  >
                    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                      <path
                        d="M1.5 2.5h5l1.5 1.5H12.5v8H1.5z"
                        stroke="currentColor"
                        strokeWidth="1.2"
                        strokeLinejoin="round"
                      />
                      <path
                        d="M7 7v3.5M5.5 9H8.5"
                        stroke="currentColor"
                        strokeWidth="1.2"
                        strokeLinecap="round"
                      />
                    </svg>
                  </button>
                </div>
                {editPathError && (
                  <p className="text-xs text-accent-text">{editPathError}</p>
                )}
              </div>

              <div className="space-y-2 pt-1">
                <div>
                  <div className="text-xs font-semibold text-fg-2">
                    {t("processes.monitorsTitle")}
                  </div>
                  <div className="text-xs text-muted">
                    {t("processes.monitorsHint")}
                  </div>
                </div>
                {monitors === null ? (
                  <p className="text-xs text-muted italic">
                    {t("processes.monitorsLoading")}
                  </p>
                ) : (
                  (() => {
                    const nums = buildDisplayNums(monitors);
                    return [...monitors]
                      .sort(
                        (a, b) =>
                          (nums.get(a.device_name) ?? 0) -
                          (nums.get(b.device_name) ?? 0),
                      )
                      .map((mon) => {
                        const dev = mon.device_name;
                        const profile = config.monitors[dev];
                        const value = overrideToValue(editOverrides[dev]);
                        const rates = supportedHz[dev]?.length
                          ? supportedHz[dev]
                          : [mon.max_hz];
                        const options = [
                          {
                            value: GLOBAL,
                            label: profile?.enabled
                              ? t("processes.hzGlobal", { hz: profile.game_hz })
                              : t("processes.hzGlobalOff"),
                          },
                          { value: KEEP, label: t("processes.hzKeep") },
                          ...[...rates]
                            .sort((a, b) => b - a)
                            .map((hz) => ({
                              value: String(hz),
                              label: `${hz} Hz`,
                            })),
                        ];
                        return (
                          <div
                            key={dev}
                            className="flex items-center gap-3 rounded-lg border border-line-subtle bg-surface px-3 py-2"
                          >
                            <div
                              className={`w-6 h-6 rounded-full flex items-center justify-center text-xs font-bold shrink-0 ${
                                value !== KEEP &&
                                (value !== GLOBAL || profile?.enabled)
                                  ? "bg-accent text-white"
                                  : "bg-chip border border-line-strong text-muted"
                              }`}
                            >
                              {nums.get(dev)}
                            </div>
                            <div className="min-w-0 flex-1">
                              <div className="text-xs font-semibold text-fg truncate">
                                {mon.friendly_name}
                              </div>
                              <div className="text-[11px] text-muted truncate">
                                {getMonitorLabel(mon, monitors, t)} ·{" "}
                                {t("monitor.upTo", { hz: mon.max_hz })}
                              </div>
                            </div>
                            <CustomSelect
                              value={value}
                              options={options}
                              onChange={(v) => setEditOverride(dev, v)}
                            />
                          </div>
                        );
                      });
                  })()
                )}
              </div>

              <div className="flex gap-2 pt-1">
                <button
                  onClick={closeEditDialog}
                  className="flex-1 py-2 border border-line-strong text-fg-3 text-sm rounded-lg hover:bg-chip transition-colors btn-press"
                >
                  {t("processes.cancel")}
                </button>
                <button
                  onClick={() => void commitEditDialog()}
                  disabled={!editName.trim()}
                  className="flex-1 py-2 btn-accent text-sm font-semibold rounded-lg transition-colors btn-press"
                >
                  {t("processes.save")}
                </button>
              </div>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
