import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ThemeContext, type Theme } from "./theme";

function applyTauriTheme(theme: Theme) {
  invoke("set_window_theme", { theme }).catch(() => {});
}

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(() => {
    const saved = (localStorage.getItem("hz-theme") ?? "system") as Theme;
    // Apply immediately — before first paint
    applyTauriTheme(saved);
    return saved;
  });

  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setSystemDark(e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  const isDark = theme === "dark" || (theme === "system" && systemDark);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", isDark);
    applyTauriTheme(theme);
  }, [isDark, theme]);

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
