import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  useUiStore,
  TEXT_SIZE_MIN_PX,
  TEXT_SIZE_MAX_PX,
  TEXT_SIZE_STEP_PX,
  ROW_SCALE_MIN,
  ROW_SCALE_MAX,
  DEFAULT_TEXT_SIZE_PX,
  DEFAULT_ROW_SCALE,
  type ThemeId,
} from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";
import { LetterAvatar } from "../../../components/LetterAvatar";
import { UserAvatar } from "../../../components/UserAvatar";
import { useAuthStore } from "../../../stores/authStore";
import { stringToColor } from "../../../utils/colors";

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

const SAMPLE_ROWS = [
  { name: "sunkhan", online: true },
  { name: "nova", online: false },
];

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
      {children}
    </div>
  );
}

/// One labelled slider. `value` is the live store value, so the preview
/// under it re-renders on every drag tick — that *is* the feedback, the
/// same way the theme cards repaint the modal.
function ScaleSlider({
  label,
  readout,
  minLabel,
  maxLabel,
  min,
  max,
  step,
  value,
  rawValue,
  defaultValue,
  onChange,
  onCommit,
  children,
}: {
  label: string;
  /// Rendered verbatim in the top-right. The two sliders measure
  /// different things — one is a real font size, the other a density
  /// multiplier — so neither owns a unit the component can assume.
  readout: string;
  minLabel: React.ReactNode;
  maxLabel: React.ReactNode;
  min: number;
  max: number;
  step: number;
  value: number;
  /// What the slider shows. `rawValue` is what's actually stored — the
  /// two differ for text size, where 0 ("theme default") has no
  /// position on a px track.
  rawValue: number;
  defaultValue: number;
  onChange: (v: number) => void;
  onCommit: () => void;
  children: React.ReactNode;
}) {
  const isDefault = Math.abs(rawValue - defaultValue) < 0.001;
  return (
    <div className="mt-6">
      <div className="mb-2.5 flex items-baseline justify-between">
        <div className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
          {label}
        </div>
        <div className="flex items-center gap-2">
          <span className="font-mono text-[10.5px] tabular-nums text-text-muted">
            {readout}
          </span>
          <button
            type="button"
            onClick={() => {
              onChange(defaultValue);
              onCommit();
            }}
            disabled={isDefault}
            className="rounded-sm px-1.5 py-0.5 text-[10.5px] font-medium text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary disabled:pointer-events-none disabled:opacity-0"
          >
            Reset
          </button>
        </div>
      </div>
      <div className="flex items-center gap-3">
        <span className="w-12 shrink-0 text-[10.5px] text-text-muted">{minLabel}</span>
        <input
          type="range"
          aria-label={label}
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          // Commit on release rather than per tick: saveSettings is
          // debounced anyway, but this keeps the disk write to one per
          // drag instead of one per pixel.
          onMouseUp={onCommit}
          onKeyUp={onCommit}
          onTouchEnd={onCommit}
          className="h-[4px] flex-1 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[14px] [&::-webkit-slider-thumb]:w-[14px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
        />
        <span className="w-12 shrink-0 text-right text-[10.5px] text-text-muted">{maxLabel}</span>
      </div>
      <div className="mt-3 overflow-hidden rounded-md border border-border-divider bg-bg-light px-3 py-2.5">
        {children}
      </div>
    </div>
  );
}

export default function AppearanceTab() {
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);
  const textSizePx = useUiStore((s) => s.textSizePx);
  const rowScale = useUiStore((s) => s.rowScale);
  const setTextSizePx = useUiStore((s) => s.setTextSizePx);
  const setRowScale = useUiStore((s) => s.setRowScale);
  const username = useAuthStore((s) => s.username);

  // The slider is a multiplier, but users think in points, so the
  // readout is the message body's actual rendered size. Measuring the
  // sample beats deriving it: --text-body is per-theme (13.5px under
  // graphite, 13px under console) and wrapped in a calc(), so reading
  // the element is the only way to get a number that's true for the
  // active theme without restating the scale here.
  const sampleRef = useRef<HTMLParagraphElement | null>(null);
  const [bodyPx, setBodyPx] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!sampleRef.current) return;
    const px = parseFloat(getComputedStyle(sampleRef.current).fontSize);
    setBodyPx(Number.isFinite(px) ? Math.round(px * 10) / 10 : null);
  }, [textSizePx, theme]);

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
      <SectionLabel>Theme</SectionLabel>
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

      <ScaleSlider
        label="Text size"
        readout={bodyPx === null ? "—" : `${bodyPx}px`}
        minLabel={<span className="text-[10px]">A</span>}
        maxLabel={<span className="text-[15px]">A</span>}
        min={TEXT_SIZE_MIN_PX}
        max={TEXT_SIZE_MAX_PX}
        step={TEXT_SIZE_STEP_PX}
        // 0 means "theme default", which has no slider position of its
        // own — park the thumb on whatever that theme actually renders.
        value={textSizePx || bodyPx || TEXT_SIZE_MIN_PX}
        rawValue={textSizePx}
        defaultValue={DEFAULT_TEXT_SIZE_PX}
        onChange={setTextSizePx}
        onCommit={saveSettings}
      >
        {/* A real message row, down to the avatar and the hashed name
            colour — same roles, same tokens, same layout as
            MessageBubble, so the sample scales exactly as the chat
            will. Signed-in user's own identity, because a preview of
            someone else's account reads as a bug. */}
        <div className="flex gap-3">
          {username ? (
            <UserAvatar username={username} size={38} />
          ) : (
            <LetterAvatar username="?" size={38} />
          )}
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span
                className="font-channel text-sender font-emphasis"
                style={{ color: stringToColor(username ?? "?") }}
              >
                {username ?? "You"}
              </span>
              <span className="font-mono text-meta font-normal tabular-nums text-text-muted">
                Today at 14:32
              </span>
            </div>
            <p
              ref={sampleRef}
              className="mt-0.5 text-body leading-[1.55] text-text-primary"
            >
              The quick brown fox jumps over the lazy dog.
            </p>
          </div>
        </div>
      </ScaleSlider>

      <ScaleSlider
        label="List density"
        readout={`${Math.round(rowScale * 100)}%`}
        step={0.05}
        minLabel="Compact"
        maxLabel="Roomy"
        min={ROW_SCALE_MIN}
        max={ROW_SCALE_MAX}
        value={rowScale}
        rawValue={rowScale}
        defaultValue={DEFAULT_ROW_SCALE}
        onChange={setRowScale}
        onCommit={saveSettings}
      >
        {/* Mirrors a members-list block — same .list-row primitive and
            the same container variables the real list publishes. */}
        <div
          className="-mx-1"
          style={
            {
              "--list-row-pad-y": "7px",
              "--list-row-pad-x": "8px",
              "--list-row-gap": "10px",
            } as React.CSSProperties
          }
        >
          {SAMPLE_ROWS.map((row) => (
            <div key={row.name} className="list-row flex items-center rounded-sm">
              <div className="relative shrink-0">
                <LetterAvatar username={row.name} size={34} />
                <span
                  className={`avatar-dot absolute -bottom-px -right-px rounded-full border-[2.5px] border-bg-light ${
                    row.online ? "bg-success" : "bg-text-muted"
                  }`}
                />
              </div>
              <span
                className={`truncate font-channel text-member ${
                  row.online ? "font-medium text-text-secondary" : "font-normal text-text-muted"
                }`}
              >
                {row.name}
              </span>
            </div>
          ))}
        </div>
      </ScaleSlider>
    </div>
  );
}
