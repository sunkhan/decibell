import { useEffect, useRef, useState } from "react";
import { useUiStore, type ThemeId } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";

/// Enough of each palette to draw its preview swatch. These duplicate
/// the authoritative values in globals.css on purpose: a swatch has to
/// paint the theme it *offers*, not the theme that's currently active,
/// and CSS gives us no way to read another selector's variables. Kept
/// deliberately small — five roles per theme — so the duplication
/// stays cheap to keep honest.
interface ThemePreview {
  chrome: string;
  sidebar: string;
  content: string;
  divider: string;
  accent: string;
  textSecondary: string;
  textMuted: string;
}

interface ThemeOption {
  id: ThemeId;
  name: string;
  mode: "Dark" | "Light";
  preview: ThemePreview;
}

const THEMES: ThemeOption[] = [
  {
    id: "graphite",
    name: "Graphite",
    mode: "Dark",
    preview: {
      chrome: "#0e1116",
      sidebar: "#14181f",
      content: "#191e26",
      divider: "rgba(255,255,255,.05)",
      accent: "#4f8cff",
      textSecondary: "#98a2b3",
      textMuted: "#8792a5",
    },
  },
  {
    id: "graphite-light",
    name: "Graphite Light",
    mode: "Light",
    preview: {
      chrome: "#eceef2",
      sidebar: "#f4f5f8",
      content: "#fbfbfd",
      divider: "rgba(15,20,30,.07)",
      accent: "#2f6fe0",
      textSecondary: "#5b6474",
      textMuted: "#5f6877",
    },
  },
  {
    id: "console",
    name: "Console",
    mode: "Dark",
    preview: {
      chrome: "#07080a",
      sidebar: "#0c0e11",
      content: "#101215",
      divider: "rgba(255,255,255,.06)",
      accent: "#7ef0a8",
      textSecondary: "#8b958f",
      textMuted: "#869089",
    },
  },
  {
    id: "console-light",
    name: "Console Light",
    mode: "Light",
    preview: {
      chrome: "#e7eae8",
      sidebar: "#eff1ef",
      content: "#f9faf9",
      divider: "rgba(10,25,18,.10)",
      accent: "#0f7a45",
      textSecondary: "#5a665f",
      textMuted: "#5c6863",
    },
  },
  {
    // Console chrome, console-light canvas — so the swatch's two left
    // columns come from one palette and its content area from the
    // other. That contrast *is* the preview.
    id: "console-split",
    name: "Console Split",
    mode: "Light",
    preview: {
      chrome: "#07080a",
      sidebar: "#0c0e11",
      content: "#f9faf9",
      divider: "rgba(10,25,18,.10)",
      accent: "#0f7a45",
      textSecondary: "#5a665f",
      textMuted: "#5c6863",
    },
  },
];

export default function AppearanceTab() {
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);

  const selectedIndex = Math.max(
    0,
    THEMES.findIndex((t) => t.id === theme),
  );
  // Roving tabindex. Arrows move focus, Space/Enter commits — the
  // manual-activation radiogroup pattern. Auto-activating on arrow
  // would repaint the entire window on every keypress.
  const [focusedIndex, setFocusedIndex] = useState(selectedIndex);
  const cardRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const pendingFocus = useRef(false);

  // Keep the roving index in step when the theme changes from
  // elsewhere (a fresh config load, another settings surface).
  useEffect(() => {
    setFocusedIndex(selectedIndex);
  }, [selectedIndex]);

  useEffect(() => {
    if (!pendingFocus.current) return;
    pendingFocus.current = false;
    cardRefs.current[focusedIndex]?.focus();
  }, [focusedIndex]);

  const commit = (id: ThemeId) => {
    if (id === theme) return;
    setTheme(id);
    saveSettings();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    let next: number | null = null;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      next = (focusedIndex + 1) % THEMES.length;
    } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      next = (focusedIndex - 1 + THEMES.length) % THEMES.length;
    } else if (e.key === "Home") {
      next = 0;
    } else if (e.key === "End") {
      next = THEMES.length - 1;
    }
    if (next === null) return;
    // Stop the arrow keys from scrolling the modal body underneath.
    e.preventDefault();
    pendingFocus.current = true;
    setFocusedIndex(next);
  };

  return (
    <div>
      <div className="mb-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
        Theme
      </div>
      <div
        role="radiogroup"
        aria-label="Theme"
        className="grid grid-cols-2 gap-3"
        onKeyDown={handleKeyDown}
      >
        {THEMES.map((option, i) => {
          const isSelected = option.id === theme;
          return (
            <button
              key={option.id}
              ref={(el) => {
                cardRefs.current[i] = el;
              }}
              type="button"
              role="radio"
              aria-checked={isSelected}
              tabIndex={i === focusedIndex ? 0 : -1}
              onClick={() => commit(option.id)}
              onFocus={() => setFocusedIndex(i)}
              className={`flex h-[132px] cursor-pointer flex-col rounded-lg border p-2.5 text-left outline-none transition-colors hover:bg-bg-lighter focus-visible:shadow-ring ${
                isSelected
                  ? "border-accent bg-bg-light shadow-ring"
                  : "border-border-divider bg-bg-light"
              }`}
            >
              <div
                // .theme-preview re-resolves the DS colour tokens from
                // the --p-* values below, so the miniature paints in
                // its own palette rather than the active one.
                className="theme-preview relative flex h-[76px] w-full overflow-hidden rounded-md"
                style={
                  {
                    "--p-chrome": option.preview.chrome,
                    "--p-sidebar": option.preview.sidebar,
                    "--p-content": option.preview.content,
                    "--p-raised": option.preview.content,
                    "--p-divider": option.preview.divider,
                    "--p-accent": option.preview.accent,
                    "--p-text-secondary": option.preview.textSecondary,
                    "--p-text-muted": option.preview.textMuted,
                  } as React.CSSProperties
                }
              >
                <div className="w-[10px] shrink-0 bg-bg-darkest" />
                <div className="w-[26px] shrink-0 border-r border-border-divider bg-bg-dark" />
                <div className="flex flex-1 flex-col justify-center gap-1.5 bg-bg-mid px-2.5">
                  <div className="h-1 w-[60%] rounded-full bg-text-secondary" />
                  <div className="h-1 w-[40%] rounded-full bg-text-muted" />
                  <div className="mt-0.5 h-3 w-[42px] rounded-sm bg-accent" />
                </div>
                {isSelected && (
                  <svg
                    className="absolute right-1.5 top-1.5 text-accent"
                    width="16"
                    height="16"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <circle cx="12" cy="12" r="10" fill="currentColor" stroke="none" />
                    <path d="M8 12.5l2.5 2.5L16 9.5" stroke="var(--p-content)" />
                  </svg>
                )}
              </div>

              <div className="mt-auto flex items-center justify-between gap-2">
                <span className="truncate text-[13px] font-medium text-text-primary">
                  {option.name}
                </span>
                <span className="shrink-0 rounded-sm bg-surface-hover px-1.5 py-0.5 font-mono text-[9.5px] uppercase tracking-[0.1em] text-text-muted">
                  {option.mode}
                </span>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
