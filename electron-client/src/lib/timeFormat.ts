// Times of day, in the clock the user picked (Settings → Appearance →
// Time format). Everything that prints a time goes through here, so the
// setting reaches messages, search results, the image viewer, the audit
// log and the "until" dates alike. Dates without a time keep the locale's
// own formatting and don't come here.
//
// Components read the setting with useTimeFormat() and pass it in, so a
// change re-renders what shows a time. The formatters are cached:
// toLocale*String with options builds a fresh ICU formatter per call
// (~40 µs), which a 150-row message list pays on every render.

import { useUiStore, type TimeFormat } from "../stores/uiStore";

export function useTimeFormat(): TimeFormat {
  return useUiStore((s) => s.timeFormat);
}

let autoIs12h: boolean | null = null;

/** The clock "auto" stands for: the hour cycle of the app's language. */
export function resolveTimeFormat(pref: TimeFormat): "12h" | "24h" {
  if (pref !== "auto") return pref;
  if (autoIs12h === null) {
    const cycle = new Intl.DateTimeFormat(undefined, { hour: "numeric" }).resolvedOptions().hourCycle;
    autoIs12h = cycle === "h11" || cycle === "h12";
  }
  return autoIs12h ? "12h" : "24h";
}

/** Options to add to any Intl / toLocale* call that shows a time of day.
 *  24-hour is `hourCycle: "h23"`, not `hour12: false`: Chromium has drawn
 *  `hour12: false` as "24:05" for five past midnight. */
export function hourOptions(pref: TimeFormat): Intl.DateTimeFormatOptions {
  return resolveTimeFormat(pref) === "12h" ? { hour12: true } : { hourCycle: "h23" };
}

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(
  kind: "clock" | "full" | "dateTime",
  pref: TimeFormat,
): Intl.DateTimeFormat {
  const clock = resolveTimeFormat(pref);
  const key = `${kind}:${clock}`;
  let f = formatters.get(key);
  if (!f) {
    const hours = hourOptions(pref);
    const options: Intl.DateTimeFormatOptions =
      kind === "clock"
        ? // "14:32" / "09:05", and "2:32 PM" rather than "02:32 PM".
          { hour: clock === "12h" ? "numeric" : "2-digit", minute: "2-digit", ...hours }
        : kind === "full"
          ? { dateStyle: "full", timeStyle: "short", ...hours }
          : { dateStyle: "medium", timeStyle: "short", ...hours };
    f = new Intl.DateTimeFormat(undefined, options);
    formatters.set(key, f);
  }
  return f;
}

function valid(date: Date): boolean {
  return !Number.isNaN(date.getTime());
}

/** "14:32" or "2:32 PM". Empty for an invalid date (format() would throw). */
export function formatClock(date: Date, pref: TimeFormat): string {
  return valid(date) ? formatter("clock", pref).format(date) : "";
}

/** The clock without AM / PM ("2:32"), for the hover time beside a grouped
 *  message: its 38 px column fits "14:32" but not "12:59 PM". The group's
 *  first message shows the full time, and the tooltip has it too. */
export function formatClockCompact(date: Date, pref: TimeFormat): string {
  if (!valid(date)) return "";
  const f = formatter("clock", pref);
  if (resolveTimeFormat(pref) === "24h") return f.format(date);
  return f
    .formatToParts(date)
    .filter((p) => p.type !== "dayPeriod")
    .map((p) => p.value)
    .join("")
    .trim();
}

/** "Thursday, November 6, 2026 at 2:32 PM", for tooltips. */
export function formatFullDateTime(date: Date, pref: TimeFormat): string {
  return valid(date) ? formatter("full", pref).format(date) : "";
}

/** "Nov 6, 2026, 14:32", for an expiry or another moment shown inline. */
export function formatDateTime(date: Date, pref: TimeFormat): string {
  return valid(date) ? formatter("dateTime", pref).format(date) : "";
}
