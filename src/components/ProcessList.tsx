import { useEffect, useState, useRef, KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  processes: string[];
  onChange: (processes: string[]) => void;
  onSave: () => void;
  saving: boolean;
}

const ICON_CACHE_KEY = "hz-process-icons";
const ICON_CACHE_MAX = 100;

function loadIconCache(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(ICON_CACHE_KEY) ?? "{}");
  } catch {
    return {};
  }
}

function saveIconToCache(name: string, icon: string) {
  const cache = loadIconCache();
  cache[name] = icon;
  // Bound the cache: drop oldest (insertion-order) entries past the cap.
  const keys = Object.keys(cache);
  if (keys.length > ICON_CACHE_MAX) {
    for (const stale of keys.slice(0, keys.length - ICON_CACHE_MAX)) {
      delete cache[stale];
    }
  }
  try {
    localStorage.setItem(ICON_CACHE_KEY, JSON.stringify(cache));
  } catch {}
}

export function ProcessList({ processes, onChange }: Props) {
  const [input, setInput] = useState("");
  const [runningProcesses, setRunningProcesses] = useState<string[]>([]);
  const [processCounts, setProcessCounts] = useState<Record<string, number>>(
    {},
  );
  const [lastSeen, setLastSeen] = useState<Record<string, number>>({});
  const [processIcons, setProcessIcons] = useState<
    Record<string, string | null>
  >(() => loadIconCache());
  const [removingProcesses, setRemovingProcesses] = useState<Set<string>>(new Set());
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerList, setPickerList] = useState<string[]>([]);
  const [pickerSearch, setPickerSearch] = useState("");
  const [pickerLoading, setPickerLoading] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    invoke<string[]>("get_running_watched")
      .then(setRunningProcesses)
      .catch(() => {});
    invoke<Record<string, number>>("get_process_counts")
      .then(setProcessCounts)
      .catch(() => {});

    const interval = setInterval(() => {
      invoke<string[]>("get_running_watched")
        .then(setRunningProcesses)
        .catch(() => {});
      invoke<Record<string, number>>("get_process_counts")
        .then(setProcessCounts)
        .catch(() => {});
    }, 3000);
    return () => clearInterval(interval);
  }, []);

  useEffect(() => {
    const cache = loadIconCache();
    for (const name of processes) {
      if (processIcons[name] || cache[name]) continue;
      invoke<string | null>("get_process_icon", { processName: name })
        .then((icon) => {
          if (icon) {
            saveIconToCache(name, icon);
            setProcessIcons((prev) => ({ ...prev, [name]: icon }));
          }
        })
        .catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [processes]);

  // Track last seen times for running processes
  useEffect(() => {
    if (runningProcesses.length > 0) {
      const now = Date.now();
      setLastSeen((prev) => {
        const next = { ...prev };
        for (const r of runningProcesses) {
          next[r.toLowerCase()] = now;
        }
        return next;
      });
    }
  }, [runningProcesses]);

  useEffect(() => {
    if (!pickerOpen) return;
    function handleClick(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [pickerOpen]);

  function openPicker() {
    if (pickerOpen) {
      setPickerOpen(false);
      return;
    }
    setPickerOpen(true);
    setPickerSearch("");
    setPickerLoading(true);
    invoke<string[]>("get_all_running_processes")
      .then((list) => setPickerList(list))
      .catch(() => setPickerList([]))
      .finally(() => setPickerLoading(false));
  }

  function pickProcess(name: string) {
    const lower = name.toLowerCase();
    if (!processes.includes(lower)) {
      onChange([...processes, lower]);
    }
    setPickerOpen(false);
  }

  function add() {
    const name = input.trim().toLowerCase();
    if (!name || processes.includes(name)) return;
    const next = [...processes, name];
    onChange(next);
    setInput("");
  }

  function remove(name: string) {
    setRemovingProcesses((prev) => new Set(prev).add(name));
    setTimeout(() => {
      onChange(processes.filter((p) => p !== name));
      setRemovingProcesses((prev) => {
        const next = new Set(prev);
        next.delete(name);
        return next;
      });
    }, 220);
  }

  function handleKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") add();
  }

  function formatLastSeen(name: string): string {
    const count = processCounts[name.toLowerCase()] ?? 0;
    const ts = lastSeen[name.toLowerCase()];
    if (!ts) return count > 0 ? `${count} mal ausgeführt` : "";
    const diffMs = Date.now() - ts;
    const diffMin = Math.floor(diffMs / 60000);
    const diffH = Math.floor(diffMin / 60);
    const diffD = Math.floor(diffH / 24);
    let ago = "";
    if (diffD >= 1) ago = `vor ${diffD} Tag`;
    else if (diffH >= 1) ago = `vor ${diffH}h`;
    else if (diffMin >= 1) ago = `vor ${diffMin} Min`;
    else ago = "gerade eben";
    return count > 0 ? `${count} mal ausgeführt · ${ago}` : ago;
  }

  const filteredPicker = pickerList.filter((p) =>
    p.toLowerCase().includes(pickerSearch.toLowerCase()),
  );

  return (
    <div className="space-y-4">
      {/* Add process card */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-1 relative z-10">
        <div className="flex items-start justify-between mb-3">
          <div>
            <h2 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
              Prozess hinzufügen
            </h2>
            <p className="text-xs text-slate-400 dark:text-slate-500 mt-0.5">
              Genauer Dateiname (z.B.{" "}
              <span className="font-mono text-slate-500 dark:text-slate-400">
                chrome.exe
              </span>
              )
            </p>
          </div>
          <div className="relative shrink-0" ref={pickerRef}>
            <button
              onClick={openPicker}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-black/10 dark:border-white/10 text-xs font-medium text-slate-600 dark:text-slate-300 bg-white dark:bg-[#2a2a2a] hover:bg-slate-50 dark:hover:bg-[#333] transition-colors btn-press"
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
                <path
                  d="M2 4l4 4 4-4"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
              Aus laufenden Prozessen wählen
            </button>
            {pickerOpen && (
              <div className="absolute right-0 top-full mt-1.5 w-72 rounded-xl border border-black/10 dark:border-white/10 bg-white dark:bg-[#242424] shadow-lg z-50 overflow-hidden dropdown-anim">
                <div className="p-2 border-b border-black/6 dark:border-white/6">
                  <input
                    autoFocus
                    type="text"
                    value={pickerSearch}
                    onChange={(e) => setPickerSearch(e.target.value)}
                    placeholder="Suchen..."
                    className="w-full px-3 py-1.5 text-xs font-mono rounded-lg bg-slate-50 dark:bg-[#2a2a2a] border border-black/8 dark:border-white/8 text-slate-800 dark:text-slate-100 placeholder-slate-400 outline-none focus:border-red-400 focus:ring-1 focus:ring-red-100 dark:focus:ring-red-900/20 transition-all"
                  />
                </div>
                <div className="max-h-56 overflow-y-auto">
                  {pickerLoading ? (
                    <p className="text-xs text-slate-400 dark:text-slate-500 text-center py-4">
                      Lädt...
                    </p>
                  ) : filteredPicker.length === 0 ? (
                    <p className="text-xs text-slate-400 dark:text-slate-500 text-center py-4">
                      Keine Prozesse gefunden
                    </p>
                  ) : (
                    filteredPicker.map((name) => {
                      const alreadyAdded = processes.includes(
                        name.toLowerCase(),
                      );
                      return (
                        <button
                          key={name}
                          disabled={alreadyAdded}
                          onClick={() => pickProcess(name)}
                          className={`w-full text-left px-3 py-2 text-xs font-mono transition-colors flex items-center justify-between gap-2 ${
                            alreadyAdded
                              ? "text-slate-300 dark:text-slate-600 cursor-default"
                              : "text-slate-700 dark:text-slate-200 hover:bg-slate-50 dark:hover:bg-[#2a2a2a]"
                          }`}
                        >
                          <span className="truncate">{name}</span>
                          {alreadyAdded && (
                            <span className="text-slate-300 dark:text-slate-600 shrink-0">
                              ✓
                            </span>
                          )}
                        </button>
                      );
                    })
                  )}
                </div>
              </div>
            )}
          </div>
        </div>
        <div className="flex gap-2">
          <div className="relative flex-1">
            <svg
              className="absolute left-3 top-1/2 -translate-y-1/2 pointer-events-none"
              width="14"
              height="14"
              viewBox="0 0 14 14"
              fill="none"
            >
              <circle
                cx="6"
                cy="6"
                r="4.5"
                stroke="#94a3b8"
                strokeWidth="1.4"
              />
              <path
                d="M9.5 9.5L12 12"
                stroke="#94a3b8"
                strokeWidth="1.4"
                strokeLinecap="round"
              />
            </svg>
            <input
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKey}
              placeholder="prozessname.exe"
              className="w-full pl-9 pr-3 py-2.5 bg-white dark:bg-[#2a2a2a] border border-black/10 dark:border-white/10 rounded-xl
                         text-sm font-mono text-slate-800 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 outline-none
                         focus:border-red-400 focus:ring-2 focus:ring-red-100 dark:focus:ring-red-900/20 transition-all"
            />
          </div>
          <button
            onClick={add}
            disabled={!input.trim()}
            className="flex items-center gap-1.5 px-4 py-2.5 bg-red-500 hover:bg-red-600
                       disabled:opacity-40 text-white text-sm font-semibold rounded-xl shadow-sm shadow-red-500/20 btn-press"
            style={{ transition: "background-color 150ms cubic-bezier(0.23,1,0.32,1), transform 140ms cubic-bezier(0.23,1,0.32,1)" }}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
              <path
                d="M6 1v10M1 6h10"
                stroke="white"
                strokeWidth="2"
                strokeLinecap="round"
              />
            </svg>
            Hinzufügen
          </button>
        </div>
      </div>

      {/* Process list */}
      <div className="rounded-2xl border border-black/8 dark:border-white/8 bg-slate-50 dark:bg-[#242424] p-5 anim-fade-up stagger-2">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-xs font-semibold text-slate-500 dark:text-slate-400 uppercase tracking-wide">
            Überwachte Prozesse ({processes.length})
          </h2>
          <span className="text-xs text-slate-400 dark:text-slate-500 flex items-center gap-1">
            <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
              <path
                d="M2 5.5l2.5 2.5L9 3"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
            Änderungen werden automatisch gespeichert
          </span>
        </div>
        {processes.length === 0 ? (
          <p className="text-sm text-slate-400 dark:text-slate-500 italic">
            Keine Prozesse konfiguriert.
          </p>
        ) : (
          <div className="space-y-2">
            {processes.map((name, idx) => {
              const isRunning = runningProcesses.some(
                (r) => r.toLowerCase() === name.toLowerCase(),
              );
              return (
                <div
                  key={name}
                  className={`flex items-center gap-3 rounded-xl px-3 py-3 border process-row ${
                    isRunning
                      ? "bg-red-50 dark:bg-red-950/30 border-red-200 dark:border-red-800/50"
                      : "bg-white dark:bg-[#2a2a2a] border-black/6 dark:border-white/6 hover:border-black/12 dark:hover:border-white/12"
                  } ${removingProcesses.has(name) ? "process-row-removing" : ""}`}
                  style={{
                    animationDelay: `${Math.min(idx, 7) * 35}ms`,
                    transition: "background-color 200ms cubic-bezier(0.23,1,0.32,1), border-color 200ms cubic-bezier(0.23,1,0.32,1)",
                  }}
                >
                  {/* Icon / play button */}
                  <div
                    className={`w-9 h-9 rounded-lg flex items-center justify-center shrink-0 overflow-hidden ${
                      processIcons[name]
                        ? "bg-transparent"
                        : isRunning
                          ? "bg-red-500 shadow-sm shadow-red-500/30"
                          : "bg-slate-200 dark:bg-slate-700"
                    }`}
                  >
                    {processIcons[name] ? (
                      <img
                        src={processIcons[name]!}
                        alt=""
                        className="w-9 h-9 object-contain"
                      />
                    ) : (
                      <svg
                        width="10"
                        height="10"
                        viewBox="0 0 10 10"
                        fill="none"
                      >
                        <path
                          d="M3 2l5 3-5 3V2z"
                          fill={isRunning ? "white" : "#94a3b8"}
                        />
                      </svg>
                    )}
                  </div>

                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-semibold font-mono text-slate-800 dark:text-slate-100 truncate select-text">
                      {name}
                    </div>
                    {isRunning ? (
                      <div className="text-xs text-slate-500 dark:text-slate-400">
                        Läuft · trigger aktiv
                      </div>
                    ) : (
                      <div className="text-xs text-slate-400 dark:text-slate-500">
                        {formatLastSeen(name)}
                      </div>
                    )}
                  </div>

                  {isRunning && (
                    <span className="text-xs font-bold bg-red-500 text-white px-2.5 py-0.5 rounded-full shrink-0">
                      • AKTIV
                    </span>
                  )}

                  <button
                    onClick={() => remove(name)}
                    className="w-6 h-6 flex items-center justify-center text-slate-300 dark:text-slate-600
                               hover:text-slate-500 dark:hover:text-slate-400 shrink-0 rounded btn-press"
                    style={{ transition: "color 150ms cubic-bezier(0.23,1,0.32,1), transform 140ms cubic-bezier(0.23,1,0.32,1)" }}
                    title="Entfernen"
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
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
