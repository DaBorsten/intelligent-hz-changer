import type { MonitorInfoExtended } from "./types";

// Windows liefert `\\.\DISPLAYn` — diese Nummer sehen Nutzer auch in den
// Windows-Anzeigeeinstellungen, also übernehmen wir sie. Linux-Connectornamen
// (`eDP-1`, `DP-2`, `HDMI-A-1`) tragen keine solche Nummer; dort nummerieren
// wir über die Reihenfolge durch, in der das Backend die Monitore liefert.
export function buildDisplayNums(
  monitors: MonitorInfoExtended[],
): Map<string, number> {
  return new Map(
    monitors.map((m, i) => {
      const win = m.device_name.match(/DISPLAY(\d+)/i);
      return [m.device_name, win ? parseInt(win[1]) : i + 1];
    }),
  );
}

/** Short tag for a device name when no enumeration is at hand (event log). */
export function deviceTag(deviceName: string): string {
  return deviceName.match(/DISPLAY(\d+)/i)?.[1] ?? deviceName;
}

export function getMonitorLabel(
  mon: MonitorInfoExtended,
  all: MonitorInfoExtended[],
  t: (k: string) => string,
): string {
  if (mon.is_primary) return t("monitor.labelPrimary");
  if (mon.is_duplicate) return t("monitor.labelClone");
  const secondary = all.filter((m) => !m.is_primary && !m.is_duplicate);
  if (secondary.length === 1) return t("monitor.labelSide");
  const primary = all.find((m) => m.is_primary);
  if (!primary) return t("monitor.labelSide");
  const dx = mon.x - primary.x;
  if (dx > 100) return t("monitor.labelRight");
  if (dx < -100) return t("monitor.labelLeft");
  return mon.y < primary.y ? t("monitor.labelAbove") : t("monitor.labelBelow");
}

/** Game Hz for a monitor never configured: its fastest mode. */
export function fallbackGameHz(supported: number[], mon?: MonitorInfoExtended) {
  return supported[supported.length - 1] ?? mon?.max_hz ?? 144;
}

/** Default Hz for a monitor never configured: 60 if it has it, else its slowest. */
export function fallbackDefaultHz(
  supported: number[],
  mon?: MonitorInfoExtended,
) {
  if (supported.includes(60)) return 60;
  return supported[0] ?? mon?.current_hz ?? 60;
}
