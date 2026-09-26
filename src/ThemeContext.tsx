import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "react";
import {
  RESOLVED_KEY,
  storedSystemDark,
  storedTheme,
  type Theme,
  ThemeContext,
} from "./theme";

/** Resolves `theme` on the native window and reports which side it landed
 * on. `null` means the platform's own `prefers-color-scheme` is trustworthy
 * (Windows/macOS) and the frontend should keep using that instead. */
function applyTauriTheme(theme: Theme) {
  return invoke<"light" | "dark" | null>("set_window_theme", { theme }).catch(
    (e) => {
      console.error("Failed to set window theme", e);
      return null;
    },
  );
}

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(storedTheme);

  const [systemDark, setSystemDark] = useState(storedSystemDark);

  // The last value we told (or were told) the resolved theme is. Lets the
  // onThemeChanged listener below tell an OS-driven switch apart from the
  // echo of our own set_window_theme call, and lets the plain matchMedia
  // listener defer to the backend once it has answered at least once
  // (matchMedia alone is unreliable on Linux — see set_window_theme).
  const lastResolved = useRef<"light" | "dark" | null>(null);

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => {
      if (lastResolved.current === null) setSystemDark(e.matches);
    };
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  const isDark = theme === "dark" || (theme === "system" && systemDark);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", isDark);
  }, [isDark]);

  useEffect(() => {
    let cancelled = false;
    applyTauriTheme(theme).then((resolved) => {
      if (cancelled || resolved == null) return;
      lastResolved.current = resolved;
      setSystemDark(resolved === "dark");
      localStorage.setItem(RESOLVED_KEY, resolved);
    });
    return () => {
      cancelled = true;
    };
  }, [theme]);

  // Catches live OS theme switches that the plain media query misses on
  // Linux (it tracks the GTK theme, not the desktop's actual dark
  // preference — see set_window_theme/linux_theme for why).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onThemeChanged(({ payload }) => {
        if (payload === lastResolved.current) return;
        lastResolved.current = payload;
        setSystemDark(payload === "dark");
        localStorage.setItem(RESOLVED_KEY, payload);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("Failed to listen for theme changes", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  function setTheme(t: Theme) {
    setThemeState(t);
    localStorage.setItem("hz-theme", t);
  }

  return (
    <ThemeContext.Provider value={{ theme, setTheme, isDark }}>
      {children}
    </ThemeContext.Provider>
  );
}
