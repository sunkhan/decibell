import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import type { MenuAnchor } from "../../../hooks/useMenuPosition";
import { Icon, Popover } from "./settingsUi";

/// The custom-color end of the role color row: a color-wheel tile that
/// opens a picker (saturation/brightness field, hue strip, hex and R/G/B
/// entry), and shows the custom color itself once one is set. Colors are
/// 0xRRGGBB, where 0 means "no color" on the wire.

interface Hsv {
  h: number; // 0–360
  s: number; // 0–1
  v: number; // 0–1
}

/// Pure black would read as "no color", so it's stored one step up.
const NEAR_BLACK = 0x010101;
/// Where the picker starts when the role has no color yet.
const START: Hsv = { h: 210, s: 0.62, v: 0.9 };
const HUE_TRACK =
  "linear-gradient(to right, #f00 0%, #ff0 16.67%, #0f0 33.33%, #0ff 50%, #00f 66.67%, #f0f 83.33%, #f00 100%)";
const WHEEL = "conic-gradient(from 90deg, #ff5b5b, #ffd34d, #57e389, #4dd8ff, #6a7dff, #e05bff, #ff5b5b)";
/// The hue thumb's radius: it travels inside the strip, not past its ends.
const THUMB = 8;

const channels = (c: number) => [(c >> 16) & 255, (c >> 8) & 255, c & 255];
export const hexOf = (c: number) => c.toString(16).padStart(6, "0").toUpperCase();
const clamp = (x: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, x));

function hsvToRgb({ h, s, v }: Hsv): number {
  const f = (n: number) => {
    const k = (n + h / 60) % 6;
    return Math.round(255 * (v - v * s * Math.max(0, Math.min(k, 4 - k, 1))));
  };
  return (f(5) << 16) | (f(3) << 8) | f(1);
}

/// Greys have no hue (and black no saturation): keep the previous ones so
/// the field doesn't jump while dragging through them.
function rgbToHsv(c: number, prev: Hsv = START): Hsv {
  const [r, g, b] = channels(c).map((x) => x / 255);
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  let h = prev.h;
  if (d) {
    h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h = (h * 60 + 360) % 360;
  }
  return { h, s: max ? d / max : prev.s, v: max };
}

/// "#3af", "3AF", "#33aaff" → 0x33aaff; anything else → null.
function parseHex(text: string): number | null {
  const t = text.trim().replace(/^#/, "");
  if (!/^([0-9a-f]{3}|[0-9a-f]{6})$/i.test(t)) return null;
  return parseInt(t.length === 3 ? t.replace(/./g, "$&$&") : t, 16);
}

/// Readable mark on a filled swatch: dark on light colors, white on the rest.
export function onSwatch(c: number): string {
  const [r, g, b] = channels(c);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 160 ? "rgb(0 0 0 / 0.75)" : "#fff";
}

export function CustomColorSwatch({
  color,
  active,
  disabled,
  onChange,
}: {
  color: number;
  /// The role's color is a custom one (set, and not a preset).
  active: boolean;
  disabled: boolean;
  onChange: (c: number) => void;
}) {
  const btn = useRef<HTMLButtonElement>(null);
  const [anchor, setAnchor] = useState<MenuAnchor | null>(null);
  const label = active ? `Custom color #${hexOf(color)}` : "Custom color";
  return (
    <>
      <button
        ref={btn}
        type="button"
        disabled={disabled}
        onClick={(e) => setAnchor(anchor ? null : anchorUnder(e.currentTarget))}
        aria-haspopup="dialog"
        aria-expanded={!!anchor}
        aria-pressed={active}
        aria-label={label}
        title={label}
        className={`relative flex h-8 w-8 items-center justify-center rounded-md transition-transform disabled:cursor-not-allowed ${
          active ? "ring-2 ring-text-primary ring-offset-2 ring-offset-bg-dark" : anchor ? "scale-110" : "hover:scale-110"
        }`}
        style={{ background: active ? `#${hexOf(color)}` : WHEEL }}
      >
        {active ? (
          <span style={{ color: onSwatch(color) }}>
            <Icon name="check" size={15} strokeWidth={3} />
          </span>
        ) : (
          <span className="flex h-5 w-5 items-center justify-center rounded-sm bg-bg-dark text-text-primary">
            <Icon name="plus" size={12} strokeWidth={2.75} />
          </span>
        )}
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} ignore={btn} className="w-64 select-none p-3">
          <ColorPicker initial={color} onChange={onChange} onDone={() => setAnchor(null)} />
        </Popover>
      )}
    </>
  );
}

/// The popover's width (w-64), so its right edge lines up with the tile's
/// and the picker hangs under the end of the color row, clear of the ring.
const PICKER_W = 256;
const anchorUnder = (el: HTMLElement): MenuAnchor => {
  const r = el.getBoundingClientRect();
  return { x: Math.round(r.right - PICKER_W), y: Math.round(r.bottom + 8) };
};

/// Pointer drags over a slider surface, in 0–1 coordinates of its box
/// (less `inset` px at each end horizontally).
function dragHandlers(onMove: (x: number, y: number) => void, inset = 0) {
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    onMove(clamp((e.clientX - r.left - inset) / (r.width - 2 * inset)), clamp((e.clientY - r.top) / r.height));
  };
  return {
    onPointerDown: (e: PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      e.currentTarget.setPointerCapture(e.pointerId);
      move(e);
    },
    onPointerMove: (e: PointerEvent<HTMLDivElement>) => {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) move(e);
    },
  };
}

/// Arrow keys step 1 unit, 10 with Shift.
const step = (e: KeyboardEvent, keys: Record<string, number>): number | null => {
  const dir = keys[e.key];
  if (dir === undefined) return null;
  e.preventDefault();
  return dir * (e.shiftKey ? 10 : 1);
};

function ColorPicker({
  initial,
  onChange,
  onDone,
}: {
  initial: number;
  onChange: (c: number) => void;
  onDone: () => void;
}) {
  const [hsv, setHsv] = useState<Hsv>(() => (initial ? rgbToHsv(initial) : START));
  /// Text being typed into a field, until it blurs; null shows the color.
  const [draft, setDraft] = useState<{ field: string; text: string } | null>(null);
  const hexRef = useRef<HTMLInputElement>(null);
  // Open on the hex code, selected, so a typed or pasted one replaces it.
  useEffect(() => {
    hexRef.current?.focus();
    hexRef.current?.select();
  }, []);
  const rgb = hsvToRgb(hsv);
  const hex = hexOf(rgb);

  const apply = (next: Hsv, exact?: number) => {
    setHsv(next);
    onChange((exact ?? hsvToRgb(next)) || NEAR_BLACK);
  };
  const applyRgb = (c: number) => apply(rgbToHsv(c, hsv), c);

  const sv = dragHandlers((x, y) => apply({ ...hsv, s: x, v: 1 - y }));
  const hue = dragHandlers((x) => apply({ ...hsv, h: x * 360 }), THUMB);

  const field = (name: string, value: string) => ({
    value: draft?.field === name ? draft.text : value,
    onFocus: (e: { currentTarget: HTMLInputElement }) => e.currentTarget.select(),
    onBlur: () => setDraft(null),
    spellCheck: false,
    autoComplete: "off",
  });
  const [r, g, b] = channels(rgb);

  return (
    <div className="flex flex-col gap-3">
      <div
        role="slider"
        tabIndex={0}
        aria-label="Saturation and brightness"
        aria-valuenow={Math.round(hsv.s * 100)}
        aria-valuetext={`Saturation ${Math.round(hsv.s * 100)}%, brightness ${Math.round(hsv.v * 100)}%`}
        {...sv}
        onKeyDown={(e) => {
          const ds = step(e, { ArrowLeft: -1, ArrowRight: 1 });
          const dv = step(e, { ArrowDown: -1, ArrowUp: 1 });
          if (ds !== null) apply({ ...hsv, s: clamp(hsv.s + ds / 100) });
          if (dv !== null) apply({ ...hsv, v: clamp(hsv.v + dv / 100) });
        }}
        className="relative h-36 cursor-crosshair touch-none rounded-md outline-none ring-1 ring-inset ring-border focus-visible:shadow-ring"
        style={{
          background: `linear-gradient(to top, #000, transparent), linear-gradient(to right, #fff, hsl(${hsv.h} 100% 50%))`,
        }}
      >
        <span
          className="pointer-events-none absolute h-4 w-4 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white ring-1 ring-black/40"
          style={{ left: `${hsv.s * 100}%`, top: `${(1 - hsv.v) * 100}%`, background: `#${hex}` }}
        />
      </div>

      <div
        role="slider"
        tabIndex={0}
        aria-label="Hue"
        aria-valuemin={0}
        aria-valuemax={360}
        aria-valuenow={Math.round(hsv.h)}
        {...hue}
        onKeyDown={(e) => {
          const d = step(e, { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1 });
          if (d !== null) apply({ ...hsv, h: clamp(hsv.h + d, 0, 360) });
        }}
        className="relative h-3 cursor-pointer touch-none rounded-full outline-none ring-1 ring-inset ring-black/10 focus-visible:shadow-ring"
        style={{ background: HUE_TRACK }}
      >
        <span
          className="pointer-events-none absolute top-1/2 h-4 w-4 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white ring-1 ring-black/40"
          style={{ left: `calc(${THUMB}px + (100% - ${2 * THUMB}px) * ${hsv.h / 360})`, background: `hsl(${hsv.h} 100% 50%)` }}
        />
      </div>

      <div className="flex gap-1.5">
        <Labelled label="Hex" className="flex-[1.75]">
          <div className="flex h-8 items-center rounded-sm border border-border bg-bg-lighter transition-all hover:border-accent/40 focus-within:border-accent focus-within:shadow-ring">
            <span className="pl-2 font-mono text-[12px] text-text-muted">#</span>
            <input
              {...field("hex", hex)}
              ref={hexRef}
              maxLength={7}
              aria-label="Hex color"
              onChange={(e) => {
                setDraft({ field: "hex", text: e.target.value });
                // Six digits apply as they're typed; three only on Enter or
                // blur, or typing "33aaff" would flash #3333AA on the way.
                const c = parseHex(e.target.value);
                if (c !== null && e.target.value.replace("#", "").length === 6) applyRgb(c);
              }}
              onBlur={() => {
                const c = draft?.field === "hex" ? parseHex(draft.text) : null;
                if (c !== null && c !== rgb) applyRgb(c);
                setDraft(null);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter") return;
                const c = draft?.field === "hex" ? parseHex(draft.text) : null;
                if (c !== null && c !== rgb) applyRgb(c);
                onDone();
              }}
              className="h-full w-full min-w-0 bg-transparent pl-0.5 pr-2 font-mono text-[12px] uppercase text-text-primary outline-none"
            />
          </div>
        </Labelled>
        {(["R", "G", "B"] as const).map((name, i) => {
          const values = [r, g, b];
          const set = (v: number) => {
            values[i] = clamp(Math.round(v), 0, 255);
            applyRgb((values[0] << 16) | (values[1] << 8) | values[2]);
          };
          return (
            <Labelled key={name} label={name} className="flex-1">
              <input
                {...field(name, String(values[i]))}
                inputMode="numeric"
                maxLength={3}
                aria-label={{ R: "Red", G: "Green", B: "Blue" }[name]}
                onChange={(e) => {
                  const text = e.target.value.replace(/\D/g, "");
                  setDraft({ field: name, text });
                  if (text) set(Number(text));
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") return onDone();
                  const d = step(e, { ArrowDown: -1, ArrowUp: 1 });
                  if (d !== null) {
                    setDraft(null);
                    set(values[i] + d);
                  }
                }}
                className="h-8 w-full min-w-0 rounded-sm border border-border bg-bg-lighter px-1 text-center font-mono text-[12px] tabular-nums text-text-primary outline-none transition-all hover:border-accent/40 focus:border-accent focus:shadow-ring"
              />
            </Labelled>
          );
        })}
      </div>
    </div>
  );
}

function Labelled({ label, className, children }: { label: string; className: string; children: ReactNode }) {
  return (
    <label className={`flex min-w-0 flex-col items-center gap-1 ${className}`}>
      <span className="w-full">{children}</span>
      <span className="font-meta text-[10.5px] font-semibold uppercase tracking-wide text-text-muted">{label}</span>
    </label>
  );
}
