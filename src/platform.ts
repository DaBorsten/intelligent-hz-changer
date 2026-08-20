/**
 * Which OS the app runs on. The user agent is the only *synchronous* platform
 * signal available (Tauri's platform API is async), and input placeholders have
 * to be right on the very first render — WebView2 always reports "Windows NT",
 * WebKitGTK on Linux does not.
 */
export const isWindows = navigator.userAgent.includes("Windows");

/**
 * Picks the Linux wording of a translation key. Placeholders, hints and browse
 * dialogs all talk about `.exe` files, which don't exist on Linux, so every key
 * passed here has a `…Linux` sibling in the translation files.
 */
export function platformKey(key: string): string {
  return isWindows ? key : `${key}Linux`;
}

/**
 * True if `path` points at a concrete executable file rather than a bare
 * process name — an `.exe` on Windows, an absolute path on Linux, where
 * executables carry no extension to recognise them by.
 */
export function looksLikeExecutablePath(path: string): boolean {
  const trimmed = path.trim();
  return isWindows
    ? trimmed.toLowerCase().endsWith(".exe")
    : trimmed.startsWith("/");
}
