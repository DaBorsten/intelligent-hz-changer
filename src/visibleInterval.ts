/** Runs `fn` every `ms` while the page is visible. A minimized window reports
 * `hidden`, so its polling stops instead of querying the backend for a UI
 * nobody sees; on becoming visible again `fn` runs once right away, then on
 * the interval. Returns a function that stops it all. */
export function visibleInterval(fn: () => void, ms: number): () => void {
  let timer: ReturnType<typeof setInterval> | null = null;
  const sync = () => {
    if (document.visibilityState === "hidden") {
      if (timer) clearInterval(timer);
      timer = null;
    } else if (!timer) {
      timer = setInterval(fn, ms);
    }
  };
  const onChange = () => {
    if (document.visibilityState !== "hidden" && !timer) fn();
    sync();
  };
  sync();
  document.addEventListener("visibilitychange", onChange);
  return () => {
    document.removeEventListener("visibilitychange", onChange);
    if (timer) clearInterval(timer);
    timer = null;
  };
}
