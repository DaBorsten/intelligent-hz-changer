import { createContext } from "react";

export type Theme = "light" | "dark" | "system";

export interface ThemeContextValue {
  theme: Theme;
  setTheme: (t: Theme) => void;
  isDark: boolean;
}

export const ThemeContext = createContext<ThemeContextValue>({
  theme: "system",
  setTheme: () => undefined,
  isDark: false,
});

/** Caches the last value the backend resolved "system" to, so the next
 * startup can paint the right theme immediately instead of flashing light
 * (the default) until the async `set_window_theme` call comes back. */
export const RESOLVED_KEY = "hz-theme-resolved";

export function getMediaQueryDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function storedTheme(): Theme {
  return (localStorage.getItem("hz-theme") as Theme | null) ?? "system";
}

export function storedSystemDark(): boolean {
  const cached = localStorage.getItem(RESOLVED_KEY);
  return cached ? cached === "dark" : getMediaQueryDark();
}

/** Sets the `dark` class from the stored theme, synchronously. Called before
 * React renders: the backend reveals the window as soon as the page has
 * loaded, and the class must already be on by then or the light body
 * background shows for a frame. */
export function applyStoredThemeClass() {
  const theme = storedTheme();
  document.documentElement.classList.toggle(
    "dark",
    theme === "dark" || (theme === "system" && storedSystemDark()),
  );
}
