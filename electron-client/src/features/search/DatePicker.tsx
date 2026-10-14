import { useMemo } from "react";
import { dayKey, dayStart } from "./searchQuery";

/// The month grid under a before: / during: / after: filter. Focus stays in
/// the search box: arrow keys there move `active` (see SearchBox), a click
/// picks. Days after today are off — nothing was posted then.

const MONTH_FMT = new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric" });
const WEEKDAY_FMT = new Intl.DateTimeFormat(undefined, { weekday: "narrow" });

/// 1 = Monday … 7 = Sunday, from the locale (Monday when unknown).
function firstWeekday(): number {
  try {
    const loc = new Intl.Locale(navigator.language) as Intl.Locale & {
      getWeekInfo?: () => { firstDay: number };
      weekInfo?: { firstDay: number };
    };
    return (loc.getWeekInfo?.() ?? loc.weekInfo)?.firstDay ?? 1;
  } catch {
    return 1;
  }
}
const FIRST = firstWeekday();

export default function DatePicker({
  active,
  onActive,
  onPick,
}: {
  /// The highlighted day, "YYYY-MM-DD".
  active: string;
  onActive: (day: string) => void;
  onPick: (day: string) => void;
}) {
  const today = dayKey(new Date());
  const focus = dayStart(active) ?? new Date();
  const year = focus.getFullYear();
  const month = focus.getMonth();

  const cells = useMemo(() => {
    const first = new Date(year, month, 1);
    // getDay(): 0 = Sunday; shift to the locale's first weekday.
    const lead = (first.getDay() - (FIRST % 7) + 7) % 7;
    const out: { key: string; day: number; inMonth: boolean }[] = [];
    for (let i = 0; i < 42; i++) {
      const d = new Date(year, month, 1 - lead + i);
      out.push({ key: dayKey(d), day: d.getDate(), inMonth: d.getMonth() === month });
    }
    // Drop a trailing week that is entirely next month.
    return out.slice(0, out[35].inMonth ? 42 : 35);
  }, [year, month]);

  const weekdays = useMemo(
    () =>
      Array.from({ length: 7 }, (_, i) =>
        // 2024-01-01 was a Monday.
        WEEKDAY_FMT.format(new Date(2024, 0, ((FIRST - 1 + i) % 7) + 1)),
      ),
    [],
  );

  const shiftMonth = (n: number) => {
    const d = new Date(year, month + n, Math.min(focus.getDate(), 28));
    const key = dayKey(d);
    onActive(key > today ? today : key);
  };

  return (
    <div className="px-2 pb-1">
      <div className="mb-1 flex items-center gap-1 px-1">
        <span className="flex-1 text-[13px] font-semibold text-text-primary">
          {MONTH_FMT.format(new Date(year, month, 1))}
        </span>
        <button
          type="button"
          onClick={() => shiftMonth(-1)}
          aria-label="Previous month"
          className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><path d="m15 18-6-6 6-6" /></svg>
        </button>
        <button
          type="button"
          onClick={() => shiftMonth(1)}
          disabled={dayKey(new Date(year, month + 1, 1)) > today}
          aria-label="Next month"
          className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary disabled:opacity-30 disabled:hover:bg-transparent"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><path d="m9 18 6-6-6-6" /></svg>
        </button>
      </div>
      <div className="grid grid-cols-7 gap-0.5" role="grid" aria-label={MONTH_FMT.format(new Date(year, month, 1))}>
        {weekdays.map((w, i) => (
          <span key={i} className="flex h-6 items-center justify-center text-[11px] font-semibold text-text-muted">
            {w}
          </span>
        ))}
        {cells.map((c) => {
          const future = c.key > today;
          const on = c.key === active;
          return (
            <button
              key={c.key}
              type="button"
              disabled={future}
              onMouseEnter={() => !future && onActive(c.key)}
              onClick={() => onPick(c.key)}
              aria-label={c.key}
              aria-selected={on}
              className={`relative flex h-8 items-center justify-center rounded-sm text-[13px] tabular-nums ${
                on
                  ? "bg-accent font-semibold text-on-accent"
                  : future
                    ? "text-text-muted opacity-30"
                    : c.inMonth
                      ? "text-text-primary hover:bg-surface-hover"
                      : "text-text-muted hover:bg-surface-hover"
              }`}
            >
              {c.day}
              {c.key === today && !on ? (
                <span className="absolute bottom-1 h-1 w-1 rounded-full bg-accent" />
              ) : null}
            </button>
          );
        })}
      </div>
    </div>
  );
}
