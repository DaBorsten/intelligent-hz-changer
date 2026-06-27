import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import type { WatchedProcess } from "../types";
import { wpName, wpKey } from "../types";

interface Props {
  processes: WatchedProcess[];
  onChange: (processes: WatchedProcess[]) => void;
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

export function ProcessList({ processes, onChange }: Props) {
  const { t } = useTranslation();
  const pickerBackdropPointerStartedOutside = useRef(false);
  const editBackdropPointerStartedOutside = useRef(false);
  const [input, setInput] = useState("");
  const [runningKeys, setRunningKeys] = useState<string[]>([]);
  const [processCounts, setProcessCounts] = useState<Record<string, number>>({});
  const [lastSeen, setLastSeen] = useState<Record<string, number>>({});
  const [now, setNow] = useState(() => Date.now());
  const [processIcons, setProcessIcons] = useState<Record<string, string | null>>(() => loadIconCache());
  const [removingProcesses, setRemovingProcesses] = useState<Set<string>>(new Set());
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerList, setPickerList] = useState<RunningProcess[]>([]);
  const [pickerSearch, setPickerSearch] = useState("");
  const [pickerLoading, setPickerLoading] = useState(false);
  const [pickerIcons, setPickerIcons] = useState<Record<string, string | null | undefined>>({});
  const [addError, setAddError] = useState("");

  const [editDialog, setEditDialog] = useState<WatchedProcess | null>(null);
  const [editName, setEditName] = useState("");
  const [editPath, setEditPath] = useState("");
  const [editPathError, setEditPathError] = useState("");

  function updateRunningKeys(keys: string[]) {
    const timestamp = Date.now();
    setNow(timestamp);
    setRunningKeys(keys);
    if (keys.length === 0) return;
    setLastSeen((prev) => {
      const next = { ...prev };
      for (const k of keys) next[k] = timestamp;
      return next;
    });
  }

  useEffect(() => {
    invoke<string[]>("get_running_watched").then(updateRunningKeys).catch(() => undefined);
    invoke<Record<string, number>>("get_process_counts").then(setProcessCounts).catch(() => undefined);

    const interval = setInterval(() => {
      invoke<string[]>("get_running_watched").then(updateRunningKeys).catch(() => undefined);
      invoke<Record<string, number>>("get_process_counts").then(setProcessCounts).catch(() => undefined);
    }, 3000);
    return () => clearInterval(interval);
  }, []);

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
        .catch(() => {});
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
          invoke<string | null>("get_process_icon", { processName: rp.name, exePath: rp.path ?? undefined })
            .then((icon) => {
              if (icon) {
                saveIconToCache(key, icon);
                setPickerIcons((prev) => ({ ...prev, [key]: icon }));
              }
            })
            .catch(() => {});
        }
      })
      .catch(() => setPickerList([]))
      .finally(() => setPickerLoading(false));
  }

  // ponytail: always use path when available — does NOT close picker
  function pickProcess(rp: RunningProcess) {
    const entry: WatchedProcess = rp.path ? { name: rp.name, path: rp.path } : rp.name;
    const key = wpKey(entry);
    if (!processes.some((p) => wpKey(p) === key)) {
      onChange([...processes, entry]);
    }
  }

  async function browseExe(onPick: (path: string, filename: string) => void) {
    const selected = await openDialog({
      filters: [{ name: t("processes.browseExeFilter"), extensions: ["exe"] }],
      multiple: false,
      title: t("processes.browseExeTitle"),
    });
    if (!selected || typeof selected !== "string") return;
    onPick(selected, filenameFromPath(selected));
  }

  async function add() {
    const raw = input.trim();
    if (!raw) return;
    setAddError("");
    // ponytail: path if contains separator, else just name
    const isPath = raw.includes("\\") || raw.includes("/");
    let normalized = raw;
    if (!normalized.toLowerCase().endsWith(".exe")) normalized += ".exe";
    const name = isPath
      ? (normalized.replace(/\\/g, "/").split("/").pop() ?? normalized)
      : normalized;
    const entry: WatchedProcess = isPath ? { name, path: normalized } : name;
    if (isPath) {
      const exists = await invoke<boolean>("check_exe_exists", { path: normalized });
      if (!exists) {
        setAddError(t("processes.notFound"));
        return;
      }
    }
    const key = wpKey(entry);
    if (processes.some((p) => wpKey(p) === key)) return;
    onChange([...processes, entry]);
    setInput("");
  }

  function remove(wp: WatchedProcess) {
    const key = wpKey(wp);
    setRemovingProcesses((prev) => new Set(prev).add(key));
    setTimeout(() => {
      onChange(processes.filter((p) => wpKey(p) !== key));
      setRemovingProcesses((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }, 220);
  }

  function openEditDialog(wp: WatchedProcess) {
    setEditDialog(wp);
    setEditName(wpName(wp));
    setEditPath(typeof wp === "string" ? "" : wp.path);
    setEditPathError("");
  }

  function closeEditDialog() {
    setEditDialog(null);
  }

  function handleEditPathChange(path: string) {
    setEditPath(path);
    setEditPathError("");
    if (path.trim().toLowerCase().endsWith(".exe")) {
      const filename = filenameFromPath(path.trim());
      if (filename) setEditName(filename);
    }
  }

  async function commitEditDialog() {
    if (!editDialog) return;
    const name = editName.trim();
    if (!name) return;
    const path = editPath.trim();
    if (path.toLowerCase().endsWith(".exe")) {
      const exists = await invoke<boolean>("check_exe_exists", { path });
      if (!exists) {
        setEditPathError(t("processes.notFound"));
        return;
      }
    }
    const originalKey = wpKey(editDialog);
    const updated: WatchedProcess = path ? { name, path } : name;
    onChange(processes.map((p) => (wpKey(p) === originalKey ? updated : p)));
    setEditDialog(null);
  }

  function handleKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") void add();
  }

  function handleBackdropPointerDown(e: PointerEvent<HTMLDivElement>, ref: { current: boolean }) {
    ref.current = e.target === e.currentTarget;
  }

  function handleBackdropPointerUp(e: PointerEvent<HTMLDivElement>, ref: { current: boolean }, onClose: () => void) {
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
      diffD >= 1 ? t("processes.agoDays", { n: diffD })
      : diffH >= 1 ? t("processes.agoHours", { n: diffH })
      : diffMin >= 1 ? t("processes.agoMinutes", { n: diffMin })
      : t("processes.agoJustNow");

    return count > 0
      ? t("processes.lastSeenWithCount", { countStr, ago })
      : t("processes.lastSeenAgoOnly", { ago });
  }

  const filteredPicker = pickerList.filter((p) =>
    p.name.toLowerCase().includes(pickerSearch.toLowerCase()),
  );
  const editPathIsExe = editPath.trim().toLowerCase().endsWith(".exe");

  return (
    <>
      <div className="space-y-4">
        {/* Add process card */}
        <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-1 relative z-10">
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-baseline gap-2">
              <h2 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
                {t("processes.addTitle")}
              </h2>
              <span className="text-xs text-slate-400 dark:text-slate-500">
                {t("processes.addHint")}
              </span>
            </div>
            <button
              onClick={openPicker}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-black/10 dark:border-white/10 text-xs font-medium text-slate-600 dark:text-slate-300 bg-white dark:bg-[#2a2a2a] hover:bg-slate-50 dark:hover:bg-[#333] transition-colors btn-press shrink-0"
            >
              {t("processes.pickRunning")}
            </button>
          </div>

          {addError && <p className="text-xs text-red-500 mb-1">{addError}</p>}
          <div className="flex gap-2">
            <div className="relative flex-1">
              <svg className="absolute left-3 top-1/2 -translate-y-1/2 pointer-events-none" width="14" height="14" viewBox="0 0 14 14" fill="none">
                <path d="M1.5 3h4l1.5 1.5H12.5v7.5H1.5z" stroke="#94a3b8" strokeWidth="1.3" strokeLinejoin="round" />
              </svg>
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKey}
                placeholder={t("processes.inputPlaceholder")}
                className="w-full pl-9 pr-3 py-2.5 bg-white dark:bg-[#2a2a2a] border border-black/10 dark:border-white/10 rounded-xl
                           text-sm font-mono text-slate-800 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 outline-none
                           focus:border-red-400 focus:ring-2 focus:ring-red-100 dark:focus:ring-red-900/20 transition-all"
              />
            </div>
            <button
              onClick={() => void browseExe((path) => setInput(path))}
              title={t("processes.browseExeTitle")}
              className="flex items-center gap-1.5 px-3 py-2.5 rounded-xl border border-black/10 dark:border-white/10 bg-white dark:bg-[#2a2a2a] text-slate-500 dark:text-slate-400 hover:bg-slate-50 dark:hover:bg-[#333] text-sm transition-colors btn-press"
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                <path d="M1.5 3h4l1.5 1.5H12.5v7.5H1.5z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" />
              </svg>
              {t("processes.browse")}
            </button>
            <button
              onClick={() => void add()}
              disabled={!input.trim()}
              className="flex items-center gap-1.5 px-4 py-2.5 bg-red-500 hover:bg-red-600
                         disabled:opacity-40 text-white text-sm font-semibold rounded-xl shadow-sm shadow-red-500/20 btn-press"
              style={{ transition: "background-color 150ms cubic-bezier(0.23,1,0.32,1), transform 140ms cubic-bezier(0.23,1,0.32,1)" }}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                <path d="M6 1v10M1 6h10" stroke="white" strokeWidth="2" strokeLinecap="round" />
              </svg>
              {t("processes.add")}
            </button>
          </div>
        </div>

        {/* Process list */}
        <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-2">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wide">
              {t("processes.watchedHeader", { count: processes.length })}
            </h2>
          </div>
          {processes.length === 0 ? (
            <p className="text-sm text-slate-400 dark:text-slate-500 italic">
              {t("processes.noProcesses")}
            </p>
          ) : (
            <div className="space-y-2">
              {processes.map((wp, idx) => {
                const key = wpKey(wp);
                const name = wpName(wp);
                const path = typeof wp === "string" ? null : wp.path;
                const iconKey = name.toLowerCase();
                const icon = processIcons[iconKey];
                const isRunning = runningKeys.some((r) => r === key);

                return (
                  <div
                    key={key}
                    className={`rounded-xl border process-row ${
                      isRunning
                        ? "bg-red-50 dark:bg-red-950/30 border-red-200 dark:border-red-800/50"
                        : "bg-white dark:bg-[#2a2a2a] border-black/6 dark:border-white/6 hover:border-black/12 dark:hover:border-white/12"
                    } ${removingProcesses.has(key) ? "process-row-removing" : ""}`}
                    style={{
                      animationDelay: `${Math.min(idx, 7) * 35}ms`,
                      transition: "background-color 200ms cubic-bezier(0.23,1,0.32,1), border-color 200ms cubic-bezier(0.23,1,0.32,1)",
                    }}
                  >
                    <div className="flex items-center gap-3 px-3 py-3">
                      <div
                        className={`w-9 h-9 rounded-lg flex items-center justify-center shrink-0 overflow-hidden ${
                          icon ? "bg-transparent" : isRunning ? "bg-red-500 shadow-sm shadow-red-500/30" : "bg-slate-200 dark:bg-slate-700"
                        }`}
                      >
                        {icon ? (
                          <img src={icon} alt="" className="w-9 h-9 object-contain" />
                        ) : (
                          <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
                            <path d="M3 2l5 3-5 3V2z" fill={isRunning ? "white" : "#94a3b8"} />
                          </svg>
                        )}
                      </div>

                      <div className="flex-1 min-w-0">
                        <div className="text-sm font-semibold font-mono text-slate-800 dark:text-slate-100 truncate select-text">
                          {name}
                        </div>
                        {path && (
                          <div className="text-xs font-mono text-slate-400 dark:text-slate-500 truncate select-text" title={path}>
                            {path}
                          </div>
                        )}
                        <div className="text-xs text-slate-400 dark:text-slate-500">
                          {formatLastSeen(wp)}
                        </div>
                      </div>

                      {isRunning && (
                        <span className="text-xs font-bold bg-red-500 text-white px-2.5 py-0.5 rounded-full shrink-0">
                          {t("processes.active")}
                        </span>
                      )}

                      <button
                        onClick={() => openEditDialog(wp)}
                        className="w-6 h-6 flex items-center justify-center text-slate-300 dark:text-slate-600
                                   hover:text-slate-500 dark:hover:text-slate-400 shrink-0 rounded btn-press"
                        style={{ transition: "color 150ms cubic-bezier(0.23,1,0.32,1), transform 140ms cubic-bezier(0.23,1,0.32,1)" }}
                        title={t("processes.editTitle")}
                      >
                        <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                          <path d="M8.5 1.5l2 2-7 7H1.5v-2l7-7z" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
                        </svg>
                      </button>

                      <button
                        onClick={() => remove(wp)}
                        className="w-6 h-6 flex items-center justify-center text-slate-300 dark:text-slate-600
                                   hover:text-slate-500 dark:hover:text-slate-400 shrink-0 rounded btn-press"
                        style={{ transition: "color 150ms cubic-bezier(0.23,1,0.32,1), transform 140ms cubic-bezier(0.23,1,0.32,1)" }}
                        title={t("processes.removeTitle")}
                      >
                        <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                          <path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
                        </svg>
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* Process picker modal */}
      {pickerOpen &&
        createPortal(
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm"
            onPointerDown={(e) => handleBackdropPointerDown(e, pickerBackdropPointerStartedOutside)}
            onPointerUp={(e) => handleBackdropPointerUp(e, pickerBackdropPointerStartedOutside, () => setPickerOpen(false))}
            onPointerCancel={() => { pickerBackdropPointerStartedOutside.current = false; }}
          >
            <div
              className="w-full max-w-sm mx-4 rounded-2xl border border-black/10 dark:border-white/10 bg-white dark:bg-[#242424] shadow-xl flex flex-col overflow-hidden"
              style={{ maxHeight: "min(520px, calc(100vh - 80px))" }}
            >
              <div className="flex items-start justify-between p-4 border-b border-black/6 dark:border-white/6 shrink-0">
                <div>
                  <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
                    {t("processes.pickerTitle")}
                  </h3>
                  <p className="text-xs text-slate-400 dark:text-slate-500 mt-0.5">
                    {t("processes.pickerHint")}
                  </p>
                </div>
                <button
                  onClick={() => setPickerOpen(false)}
                  className="w-6 h-6 flex items-center justify-center text-slate-400 hover:text-slate-600 dark:hover:text-slate-300 rounded transition-colors ml-2 shrink-0"
                >
                  <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                    <path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
                  </svg>
                </button>
              </div>
              <div className="p-3 border-b border-black/6 dark:border-white/6 shrink-0">
                <div className="relative">
                  <svg className="absolute left-2.5 top-1/2 -translate-y-1/2 pointer-events-none text-slate-400" width="12" height="12" viewBox="0 0 12 12" fill="none">
                    <circle cx="5" cy="5" r="3.5" stroke="currentColor" strokeWidth="1.3" />
                    <path d="M8 8l2 2" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
                  </svg>
                  <input
                    autoFocus
                    type="text"
                    value={pickerSearch}
                    onChange={(e) => setPickerSearch(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Escape") setPickerOpen(false); }}
                    placeholder={t("processes.pickerSearch")}
                    className="w-full pl-8 pr-3 py-2 text-xs rounded-lg bg-slate-50 dark:bg-[#2a2a2a] border border-black/8 dark:border-white/8 text-slate-800 dark:text-slate-100 placeholder-slate-400 outline-none focus:border-red-400 focus:ring-1 focus:ring-red-100 dark:focus:ring-red-900/20 transition-all"
                  />
                </div>
              </div>
              <div className="overflow-y-auto flex-1">
                {pickerLoading ? (
                  <p className="text-xs text-slate-400 dark:text-slate-500 text-center py-6">
                    {t("processes.loading")}
                  </p>
                ) : filteredPicker.length === 0 ? (
                  <p className="text-xs text-slate-400 dark:text-slate-500 text-center py-6">
                    {t("processes.noRunning")}
                  </p>
                ) : (
                  filteredPicker.map((rp) => {
                    const alreadyAdded = processes.some(
                      (p) => wpName(p).toLowerCase() === rp.name.toLowerCase(),
                    );
                    const iconKey = rp.name.toLowerCase();
                    const icon = pickerIcons[iconKey] ?? loadIconCache()[iconKey] ?? null;
                    return (
                      <div key={rp.name} className="flex items-center gap-3 px-4 py-2.5 hover:bg-slate-50 dark:hover:bg-[#2a2a2a] transition-colors">
                        <div className={`w-8 h-8 flex items-center justify-center shrink-0 overflow-hidden ${icon ? "" : "rounded-lg bg-slate-100 dark:bg-slate-700"}`}>
                          {icon ? (
                            <img src={icon} alt="" className="w-8 h-8 object-contain" />
                          ) : (
                            <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                              <rect x="1.5" y="2.5" width="13" height="11" rx="2" stroke="#94a3b8" strokeWidth="1.3" />
                              <path d="M1.5 6h13" stroke="#94a3b8" strokeWidth="1.1" />
                              <circle cx="4" cy="4.25" r="0.8" fill="#94a3b8" />
                              <circle cx="6.5" cy="4.25" r="0.8" fill="#94a3b8" />
                              <path d="M4.5 9l2.5 1.5-2.5 1.5" stroke="#94a3b8" strokeWidth="1.1" strokeLinecap="round" strokeLinejoin="round" />
                              <path d="M9 11.5h2" stroke="#94a3b8" strokeWidth="1.1" strokeLinecap="round" />
                            </svg>
                          )}
                        </div>
                        <div className="flex-1 min-w-0">
                          <div className={`text-xs font-semibold font-mono truncate ${alreadyAdded ? "text-slate-400 dark:text-slate-500" : "text-slate-800 dark:text-slate-100"}`}>
                            {rp.name}
                          </div>
                          {rp.path && (
                            <div className="text-[10px] font-mono text-slate-400 dark:text-slate-500 truncate mt-0.5" title={rp.path}>
                              {rp.path}
                            </div>
                          )}
                        </div>
                        {alreadyAdded ? (
                          <span className="flex items-center gap-1 text-xs text-emerald-500 dark:text-emerald-400 shrink-0 font-medium">
                            <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                              <path d="M2 6l3 3 5-5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                            </svg>
                            {t("processes.added")}
                          </span>
                        ) : (
                          <button
                            onClick={() => pickProcess(rp)}
                            className="flex items-center gap-1 text-xs text-slate-500 dark:text-slate-400 hover:text-red-500 dark:hover:text-red-400 shrink-0 transition-colors font-medium btn-press"
                          >
                            <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
                              <path d="M5.5 1v9M1 5.5h9" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
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
            onPointerDown={(e) => handleBackdropPointerDown(e, editBackdropPointerStartedOutside)}
            onPointerUp={(e) => handleBackdropPointerUp(e, editBackdropPointerStartedOutside, closeEditDialog)}
            onPointerCancel={() => { editBackdropPointerStartedOutside.current = false; }}
          >
            <div className="w-full max-w-sm mx-4 rounded-2xl border border-black/10 dark:border-white/10 bg-white dark:bg-[#242424] shadow-xl p-5 space-y-4">
              <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
                {t("processes.editProcess")}
              </h3>

              <div className="space-y-1">
                <label className="text-xs text-slate-500 dark:text-slate-400">
                  {t("processes.processName")}
                </label>
                <input
                  autoFocus={!editPathIsExe}
                  type="text"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  readOnly={editPathIsExe}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void commitEditDialog();
                    if (e.key === "Escape") closeEditDialog();
                  }}
                  placeholder={t("processes.namePlaceholder")}
                  className={`w-full px-3 py-2 bg-slate-50 dark:bg-[#2a2a2a] border border-black/10 dark:border-white/10 rounded-xl
                           text-sm font-mono text-slate-800 dark:text-slate-100 outline-none
                           focus:border-red-400 focus:ring-2 focus:ring-red-100 dark:focus:ring-red-900/20 transition-all ${
                             editPathIsExe ? "cursor-not-allowed opacity-75" : ""
                           }`}
                />
              </div>

              <div className="space-y-1">
                <label className="text-xs text-slate-500 dark:text-slate-400">
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
                    placeholder={t("processes.pathPlaceholder")}
                    className={`flex-1 px-3 py-2 bg-slate-50 dark:bg-[#2a2a2a] border rounded-xl
                             text-xs font-mono text-slate-600 dark:text-slate-300 placeholder-slate-300 dark:placeholder-slate-600 outline-none
                             focus:ring-2 transition-all ${
                               editPathError
                                 ? "border-red-400 focus:border-red-400 focus:ring-red-100 dark:focus:ring-red-900/20"
                                 : "border-black/8 dark:border-white/8 focus:border-red-400 focus:ring-red-100 dark:focus:ring-red-900/20"
                             }`}
                  />
                  <button
                    onClick={() => void browseExe((path, filename) => {
                      handleEditPathChange(path);
                      if (filename) setEditName(filename);
                    })}
                    title={t("processes.browseExeTitle")}
                    className="px-3 py-2 rounded-xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#2a2a2a] text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-[#333] transition-colors btn-press"
                  >
                    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                      <path d="M1.5 2.5h5l1.5 1.5H12.5v8H1.5z" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" />
                      <path d="M7 7v3.5M5.5 9H8.5" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
                    </svg>
                  </button>
                </div>
                {editPathError && (
                  <p className="text-xs text-red-500">{editPathError}</p>
                )}
              </div>

              <div className="flex gap-2 pt-1">
                <button
                  onClick={closeEditDialog}
                  className="flex-1 py-2 border border-black/10 dark:border-white/10 text-slate-600 dark:text-slate-300 text-sm rounded-xl hover:bg-slate-50 dark:hover:bg-[#2a2a2a] transition-colors btn-press"
                >
                  {t("processes.cancel")}
                </button>
                <button
                  onClick={() => void commitEditDialog()}
                  disabled={!editName.trim()}
                  className="flex-1 py-2 bg-red-500 hover:bg-red-600 disabled:opacity-40 text-white text-sm font-semibold rounded-xl transition-colors btn-press"
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
