import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

/// A dropdown picker that replaces the native <select>, whose popup is drawn
/// by the OS and ignores the theme. Select-only combobox (WAI-ARIA 1.2): focus
/// stays on the trigger — or the search field — and the listbox follows via
/// aria-activedescendant. Keyboard parity with the native control: arrows,
/// Home / End, PageUp / PageDown, Enter / Space, type-to-jump, Escape (which
/// doesn't reach the modal underneath), Tab closes.
///
/// The popup is portaled to <body> with fixed positioning so a scrolling or
/// overflow-hidden modal can't clip it; React events still bubble through the
/// component tree, so modal click handlers treat it as inside. Each theme
/// family restyles it in globals.css (`.dsel-*`).

export interface SelectOption<T extends string | number> {
  value: T;
  label: string;
  /// Muted second line.
  hint?: string;
  /// Leading glyph — a device icon, a role colour dot.
  icon?: ReactNode;
  disabled?: boolean;
}

export interface SelectGroup<T extends string | number> {
  label: string;
  options: SelectOption<T>[];
}

interface Props<T extends string | number> {
  /// null shows the placeholder (nothing chosen yet, or an action picker).
  value: T | null;
  onChange: (value: T) => void;
  options: SelectOption<T>[] | SelectGroup<T>[];
  placeholder?: string;
  /// md: form fields (40px). sm: inline / toolbar pickers.
  size?: "md" | "sm";
  /// field: an input on a card. sunken: a control on a raised panel
  /// (the composer's code-block toolbar).
  variant?: "field" | "sunken";
  disabled?: boolean;
  /// A filter field above the list. Defaults to on past 10 options.
  searchable?: boolean;
  /// Width / margin utilities for the trigger (it is block-level, full width
  /// of its container, unless this says otherwise).
  className?: string;
  /// The popup is at least the trigger's width; this widens it for a narrow
  /// trigger with long labels.
  menuMinWidth?: number;
  /// Leading glyph inside the trigger.
  icon?: ReactNode;
  id?: string;
  "aria-label"?: string;
}

const MARGIN = 8;
const GAP = 4;
const MAX_MENU_H = 320;
const TYPEAHEAD_MS = 600;
const PAGE = 8;

function isGrouped<T extends string | number>(
  o: SelectOption<T>[] | SelectGroup<T>[],
): o is SelectGroup<T>[] {
  return o.length > 0 && "options" in o[0];
}

interface Placement {
  left: number;
  width: number;
  maxHeight: number;
  top?: number;
  bottom?: number;
  side: "below" | "above";
}

export default function Select<T extends string | number>({
  value,
  onChange,
  options,
  placeholder = "Select…",
  size = "md",
  variant = "field",
  disabled,
  searchable,
  className = "",
  menuMinWidth,
  icon,
  id,
  "aria-label": ariaLabel,
}: Props<T>) {
  const uid = useId();
  const listId = `${uid}-list`;
  const optionId = (i: number) => `${uid}-opt-${i}`;
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [query, setQuery] = useState("");
  const [placement, setPlacement] = useState<Placement | null>(null);
  const typeahead = useRef({ buffer: "", at: 0 });

  const groups: SelectGroup<T>[] = useMemo(
    () => (isGrouped(options) ? options : [{ label: "", options }]),
    [options],
  );
  const all = useMemo(() => groups.flatMap((g) => g.options), [groups]);
  const withSearch = searchable ?? all.length > 10;
  const selected = all.find((o) => o.value === value) ?? null;

  // The visible list: groups filtered by the query, flattened for keyboard
  // navigation (`flat` indexes are what `active` points into).
  const { visibleGroups, flat } = useMemo(() => {
    const q = query.trim().toLowerCase();
    const match = (o: SelectOption<T>) =>
      !q || o.label.toLowerCase().includes(q) || (o.hint ?? "").toLowerCase().includes(q);
    const visibleGroups = groups
      .map((g) => ({ label: g.label, options: g.options.filter(match) }))
      .filter((g) => g.options.length > 0);
    return { visibleGroups, flat: visibleGroups.flatMap((g) => g.options) };
  }, [groups, query]);

  const enabledFrom = useCallback(
    (start: number, step: 1 | -1): number => {
      for (let i = start; i >= 0 && i < flat.length; i += step) {
        if (!flat[i].disabled) return i;
      }
      return -1;
    },
    [flat],
  );

  const openMenu = () => {
    if (disabled || open) return;
    // The query is "" here (close clears it), so `flat` is `all`.
    const sel = all.findIndex((o) => o.value === value);
    setActive(sel >= 0 && !all[sel].disabled ? sel : enabledFromAll(all));
    setOpen(true);
  };

  const close = useCallback((refocus = true) => {
    setOpen(false);
    setPlacement(null);
    setQuery("");
    if (refocus) triggerRef.current?.focus({ preventScroll: true });
  }, []);

  const choose = (i: number) => {
    const opt = flat[i];
    if (!opt || opt.disabled) return;
    close();
    if (opt.value !== value) onChange(opt.value);
  };

  // A query change re-targets the first match.
  useEffect(() => {
    if (open) setActive(enabledFrom(0, 1));
  }, [query]); // eslint-disable-line react-hooks/exhaustive-deps

  // Place the popup under (or, short of room, over) the trigger before the
  // first paint, then keep it there while it's open.
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const t = triggerRef.current?.getBoundingClientRect();
      const menu = menuRef.current;
      if (!t || !menu) return;
      const natural = Math.min(MAX_MENU_H, menu.scrollHeight);
      const below = window.innerHeight - t.bottom - GAP - MARGIN;
      const above = t.top - GAP - MARGIN;
      const side = below >= natural || below >= above ? "below" : "above";
      const width = Math.max(t.width, menuMinWidth ?? 0);
      const left = Math.min(Math.max(MARGIN, t.left), window.innerWidth - width - MARGIN);
      const next: Placement = {
        left,
        width,
        side,
        maxHeight: Math.min(MAX_MENU_H, side === "below" ? below : above),
        ...(side === "below"
          ? { top: t.bottom + GAP }
          : { bottom: window.innerHeight - t.top + GAP }),
      };
      setPlacement((p) =>
        p && p.left === next.left && p.width === next.width && p.top === next.top &&
        p.bottom === next.bottom && p.maxHeight === next.maxHeight ? p : next,
      );
    };
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [open, menuMinWidth, visibleGroups]);

  // Dismiss: a press outside, the window losing focus, or anything but the
  // list itself scrolling (the trigger would slide out from under it —
  // native selects close too).
  useEffect(() => {
    if (!open) return;
    const inside = (n: EventTarget | null) =>
      n instanceof Node && (menuRef.current?.contains(n) || triggerRef.current?.contains(n));
    const onDown = (e: MouseEvent) => {
      if (!inside(e.target)) close(false);
    };
    const onScroll = (e: Event) => {
      if (!(e.target instanceof Node && menuRef.current?.contains(e.target))) close(false);
    };
    const onBlur = () => close(false);
    document.addEventListener("mousedown", onDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [open, close]);

  // Search field takes focus as the popup opens.
  useEffect(() => {
    if (open && withSearch) searchRef.current?.focus({ preventScroll: true });
  }, [open, withSearch]);

  // Keep the active option in view: centred on open, nearest after that.
  const firstScroll = useRef(true);
  useEffect(() => {
    if (!open) {
      firstScroll.current = true;
      return;
    }
    if (!placement || active < 0) return;
    const el = document.getElementById(optionId(active));
    el?.scrollIntoView({ block: firstScroll.current ? "center" : "nearest" });
    firstScroll.current = false;
  }, [open, placement, active]); // eslint-disable-line react-hooks/exhaustive-deps

  const jumpTo = (ch: string) => {
    const now = Date.now();
    const t = typeahead.current;
    t.buffer = now - t.at > TYPEAHEAD_MS ? ch : t.buffer + ch;
    t.at = now;
    // "bbb" cycles through the b's; "be" narrows to labels starting "be".
    const same = t.buffer.split("").every((c) => c === t.buffer[0]);
    const needle = same ? t.buffer[0] : t.buffer;
    const from = same ? active + 1 : Math.max(active, 0);
    for (let k = 0; k < flat.length; k++) {
      const i = (from + k) % flat.length;
      if (!flat[i].disabled && flat[i].label.toLowerCase().startsWith(needle)) {
        setActive(i);
        return;
      }
    }
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (disabled) return;
    const key = e.key;
    if (!open) {
      if (key === "ArrowDown" || key === "ArrowUp" || key === "Enter" || key === " ") {
        e.preventDefault();
        openMenu();
      }
      return;
    }
    const fromSearch = e.target === searchRef.current;
    switch (key) {
      case "ArrowDown": {
        e.preventDefault();
        const n = enabledFrom(active + 1, 1);
        if (n >= 0) setActive(n);
        return;
      }
      case "ArrowUp": {
        e.preventDefault();
        const n = enabledFrom(active - 1, -1);
        if (n >= 0) setActive(n);
        return;
      }
      case "Home":
      case "End":
        if (fromSearch) return; // caret movement in the field
        e.preventDefault();
        setActive(key === "Home" ? enabledFrom(0, 1) : enabledFrom(flat.length - 1, -1));
        return;
      case "PageDown":
      case "PageUp": {
        e.preventDefault();
        const step = key === "PageDown" ? 1 : -1;
        const target = Math.min(flat.length - 1, Math.max(0, active + step * PAGE));
        const n = enabledFrom(target, step === 1 ? -1 : 1);
        if (n >= 0) setActive(n);
        return;
      }
      case "Enter":
        e.preventDefault();
        choose(active);
        return;
      case " ":
        if (fromSearch) return;
        e.preventDefault();
        if (typeahead.current.buffer && Date.now() - typeahead.current.at < TYPEAHEAD_MS) {
          jumpTo(" ");
        } else {
          choose(active);
        }
        return;
      case "Escape":
        // Ours, not the modal's: it listens on window, after us.
        e.preventDefault();
        e.stopPropagation();
        close();
        return;
      case "Tab":
        // Back to the trigger first, so the default Tab moves on from it
        // (not from a search field at the end of <body>).
        close();
        return;
      default:
        if (!fromSearch && key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
          jumpTo(key.toLowerCase());
        }
    }
  };

  const triggerSize =
    size === "sm"
      ? "gap-1.5 px-2.5 py-1.5 text-[12px] leading-[16px]"
      : "gap-2 px-3 py-2.5 text-[13px] leading-[18px]";
  const triggerTone =
    variant === "sunken" ? "bg-bg-darkest text-text-secondary" : "bg-bg-lighter text-text-primary";

  let flatIndex = -1;
  const menuStyle: CSSProperties = placement
    ? {
        position: "fixed",
        left: placement.left,
        width: placement.width,
        top: placement.top,
        bottom: placement.bottom,
        maxHeight: placement.maxHeight,
      }
    : { position: "fixed", left: 0, top: 0, opacity: 0, pointerEvents: "none" };

  return (
    <>
      <button
        ref={triggerRef}
        id={id}
        type="button"
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-activedescendant={open && !withSearch && active >= 0 ? optionId(active) : undefined}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => (open ? close() : openMenu())}
        onKeyDown={onKeyDown}
        data-size={size}
        className={`dsel-trigger group/dsel flex w-full min-w-0 items-center rounded-md border border-border text-left font-normal normal-case tracking-normal outline-none transition-all hover:border-accent/40 focus-visible:border-accent focus-visible:shadow-ring aria-expanded:border-accent aria-expanded:shadow-ring disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:border-border ${triggerSize} ${triggerTone} ${className}`}
      >
        {icon && <span className="flex shrink-0 text-text-muted">{icon}</span>}
        {selected?.icon && <span className="flex shrink-0">{selected.icon}</span>}
        <span className={`min-w-0 flex-1 truncate ${selected ? "" : "text-text-muted"}`}>
          {selected ? selected.label : placeholder}
        </span>
        <Chevron size={size} />
      </button>
      {open &&
        createPortal(
          <div
            ref={menuRef}
            data-side={placement?.side ?? "below"}
            data-size={size}
            style={menuStyle}
            onMouseDown={(e) => {
              // Keep focus where it is (trigger or search field).
              if (e.target !== searchRef.current) e.preventDefault();
            }}
            className="dsel-menu z-[150] flex flex-col overflow-hidden rounded-lg border border-border bg-bg-light shadow-float"
          >
            {withSearch && (
              <div className="dsel-search flex shrink-0 items-center gap-2 border-b border-border-divider px-2.5 py-2">
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.25" strokeLinecap="round" className="shrink-0 text-text-muted">
                  <circle cx="11" cy="11" r="7" />
                  <path d="M20 20l-3.5-3.5" />
                </svg>
                <input
                  ref={searchRef}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={onKeyDown}
                  role="combobox"
                  aria-expanded
                  aria-controls={listId}
                  aria-activedescendant={active >= 0 ? optionId(active) : undefined}
                  aria-label="Filter"
                  placeholder="Filter…"
                  spellCheck={false}
                  className="min-w-0 flex-1 bg-transparent text-[13px] text-text-primary outline-none placeholder:text-text-muted"
                />
              </div>
            )}
            <div
              ref={listRef}
              id={listId}
              role="listbox"
              aria-label={ariaLabel}
              className="min-h-0 flex-1 overflow-y-auto p-1"
            >
              {visibleGroups.length === 0 && (
                <div className="px-2.5 py-2 text-[12px] text-text-muted">No matches</div>
              )}
              {visibleGroups.map((g, gi) => (
                <div key={`${g.label}-${gi}`} role={g.label ? "group" : undefined} aria-label={g.label || undefined}>
                  {g.label && (
                    <div className="dsel-group px-2.5 pb-1 pt-2 font-section text-[11px] font-semibold uppercase tracking-section text-text-muted">
                      {g.label}
                    </div>
                  )}
                  {g.options.map((o) => {
                    flatIndex += 1;
                    const i = flatIndex;
                    const isSel = o.value === value;
                    return (
                      <div
                        key={String(o.value)}
                        id={optionId(i)}
                        role="option"
                        aria-selected={isSel}
                        aria-disabled={o.disabled || undefined}
                        data-active={i === active || undefined}
                        onMouseMove={() => {
                          if (i !== active && !o.disabled) setActive(i);
                        }}
                        onClick={() => choose(i)}
                        className={`dsel-option relative flex items-center gap-2 rounded-md ${
                          size === "sm" ? "px-2 py-1.5 text-[12px]" : "px-2.5 py-2 text-[13px]"
                        } text-text-secondary ${o.disabled ? "opacity-45" : "cursor-pointer"}`}
                      >
                        <span className="dsel-marker" aria-hidden>›</span>
                        <CheckGlyph />
                        {o.icon && <span className="flex shrink-0">{o.icon}</span>}
                        <span className="flex min-w-0 flex-1 flex-col">
                          <span className="dsel-label truncate">{o.label}</span>
                          {o.hint && <span className="dsel-hint truncate text-[11px] text-text-muted">{o.hint}</span>}
                        </span>
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}

function enabledFromAll<T extends string | number>(all: SelectOption<T>[]): number {
  return all.findIndex((o) => !o.disabled);
}

function Chevron({ size }: { size: "md" | "sm" }) {
  const px = size === "sm" ? 10 : 12;
  return (
    <>
      <svg
        width={px}
        height={px}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        className="dsel-chevron shrink-0 text-text-muted transition-transform duration-150 group-aria-expanded/dsel:rotate-180 group-aria-expanded/dsel:text-accent"
      >
        <path d="M6 9l6 6 6-6" />
      </svg>
      {/* console: a typed caret instead of the drawn chevron */}
      <span className="dsel-glyph shrink-0 text-text-muted group-aria-expanded/dsel:text-accent" aria-hidden>
        ▾
      </span>
    </>
  );
}

function CheckGlyph() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      className="dsel-check shrink-0"
    >
      <path d="M5 12.5l4.5 4.5L19 7.5" />
    </svg>
  );
}
