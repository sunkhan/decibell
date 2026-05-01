import { useEffect, useMemo, useRef } from "react";
import { useVoiceStatsStore, type ConnectionStatsSample } from "../../stores/voiceStatsStore";

interface Props {
  /** Click-anywhere-outside handler to close the popover. */
  onClose: () => void;
}

// Inner chart width is popover width (260) minus container side padding
// (px-2 = 8px each side), so the SVG drawing area stays inside the bg.
const WIDTH = 244;
const HEIGHT = 120;
const PADDING_X = 8;
const PADDING_TOP = 12;
const PADDING_BOTTOM = 16;
const WINDOW_MS = 5 * 60 * 1000;

const PING_COLOR = "rgb(56, 143, 255)";
const LOSS_COLOR = "rgb(255, 92, 88)";

function formatPing(p: number | null): string {
  return p == null ? "—" : `${p} ms`;
}
function formatLoss(p: number): string {
  return p < 0.1 ? "0%" : p < 10 ? `${p.toFixed(1)}%` : `${Math.round(p)}%`;
}

/** Build an SVG `M ... L ...` path from the sample series, mapping each
 *  sample to the chart area via the supplied y-extractor + y-scale. */
function buildPath(
  samples: ConnectionStatsSample[],
  now: number,
  extract: (s: ConnectionStatsSample) => number | null,
  yMax: number,
): string {
  const w = WIDTH - PADDING_X * 2;
  const h = HEIGHT - PADDING_TOP - PADDING_BOTTOM;
  let d = "";
  let pen_up = true;
  for (const s of samples) {
    const v = extract(s);
    if (v == null) {
      pen_up = true;
      continue;
    }
    const tNorm = 1 - (now - s.ts) / WINDOW_MS;   // 0 at left edge, 1 at right
    if (tNorm < 0 || tNorm > 1) continue;
    const x = PADDING_X + tNorm * w;
    const yNorm = Math.min(1, Math.max(0, v / yMax));
    const y = PADDING_TOP + (1 - yNorm) * h;
    d += pen_up
      ? `M ${x.toFixed(1)} ${y.toFixed(1)}`
      : ` L ${x.toFixed(1)} ${y.toFixed(1)}`;
    pen_up = false;
  }
  return d;
}

export default function ConnectionStatsPopover({ onClose }: Props) {
  const samples = useVoiceStatsStore((s) => s.samples);
  const popoverRef = useRef<HTMLDivElement | null>(null);

  // Click-outside / Escape closes the popover.
  useEffect(() => {
    const onMouseDown = (e: MouseEvent) => {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // Pin "now" once per render so both lines + axis labels share a frame.
  const now = useMemo(() => Date.now(), [samples]);

  // Chart y-scale: ping uses an adaptive ceiling so a 30ms session
  // doesn't render against a 200ms axis. Floor at 100ms so 5/10/20ms
  // pings still get visible chart real estate. Loss is fixed 0–100%.
  const pingMax = useMemo(() => {
    let max = 100;
    for (const s of samples) {
      if (s.pingMs != null && s.pingMs > max) max = s.pingMs;
    }
    return Math.ceil(max / 50) * 50; // round up to nearest 50ms tick
  }, [samples]);

  const pingPath = useMemo(
    () => buildPath(samples, now, (s) => s.pingMs, pingMax),
    [samples, now, pingMax],
  );
  const lossPath = useMemo(
    () => buildPath(samples, now, (s) => s.lossPct, 100),
    [samples, now],
  );

  const latest = samples.length > 0 ? samples[samples.length - 1] : null;
  const latestPing = latest?.pingMs ?? null;
  const latestLoss = latest?.lossPct ?? 0;

  // Average over the visible window — handy "is my connection bad on
  // average or just spiking" answer at a glance.
  const avgPing = useMemo(() => {
    let sum = 0, count = 0;
    for (const s of samples) {
      if (s.pingMs != null) { sum += s.pingMs; count += 1; }
    }
    return count > 0 ? Math.round(sum / count) : null;
  }, [samples]);
  const avgLoss = useMemo(() => {
    if (samples.length === 0) return 0;
    let sum = 0;
    for (const s of samples) sum += s.lossPct;
    return sum / samples.length;
  }, [samples]);

  return (
    <div
      ref={popoverRef}
      // Anchored to the LEFT edge of the ping button (which sits in the
      // channel sidebar on the left of the client) and extending rightward
      // into the chat area — the previous right-anchored variant pushed the
      // popover off the left edge of the screen on narrow sidebars.
      // bg-bg-lighter + border for opaque elevation over the user panel
      // (which is bg-bg-darkest); bg-bg-elevated isn't a defined token so it
      // had been rendering transparent.
      className="absolute bottom-[calc(100%+6px)] left-0 z-50 w-[260px] rounded-lg border border-border bg-bg-lighter shadow-[0_8px_24px_rgba(0,0,0,0.45)]"
      onClick={(e) => e.stopPropagation()}
    >
      <div className="border-b border-border-divider px-3 py-2.5">
        <div className="text-[11px] font-semibold uppercase tracking-wider text-text-muted">
          Connection
        </div>
        <div className="mt-1 flex items-center justify-between text-[12.5px]">
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ background: PING_COLOR }} />
            <span className="text-text-secondary">Ping</span>
            <span className="font-semibold text-text-primary tabular-nums">
              {formatPing(latestPing)}
            </span>
            {avgPing != null && avgPing !== latestPing && (
              <span className="text-text-muted tabular-nums">
                (avg {avgPing} ms)
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ background: LOSS_COLOR }} />
            <span className="text-text-secondary">Loss</span>
            <span className="font-semibold text-text-primary tabular-nums">
              {formatLoss(latestLoss)}
            </span>
          </div>
        </div>
      </div>

      <div className="px-2 pt-2 pb-1">
        <svg
          width={WIDTH}
          height={HEIGHT}
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          className="block"
        >
          {/* Background grid: faint horizontal at 25/50/75% */}
          {[0.25, 0.5, 0.75].map((frac) => {
            const y = PADDING_TOP + (HEIGHT - PADDING_TOP - PADDING_BOTTOM) * frac;
            return (
              <line
                key={frac}
                x1={PADDING_X}
                x2={WIDTH - PADDING_X}
                y1={y}
                y2={y}
                stroke="rgba(255,255,255,0.05)"
                strokeWidth={1}
              />
            );
          })}

          {/* Y-axis ticks for ping (left) — sparse, just top + middle */}
          <text
            x={PADDING_X + 2}
            y={PADDING_TOP + 8}
            fill="rgba(255,255,255,0.32)"
            fontSize="9"
            fontFamily="inherit"
          >
            {pingMax} ms
          </text>

          {/* X-axis label */}
          <text
            x={WIDTH / 2}
            y={HEIGHT - 3}
            textAnchor="middle"
            fill="rgba(255,255,255,0.32)"
            fontSize="9"
            fontFamily="inherit"
          >
            last 5 min
          </text>

          {/* Lines */}
          {pingPath && (
            <path
              d={pingPath}
              fill="none"
              stroke={PING_COLOR}
              strokeWidth={1.6}
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          )}
          {lossPath && (
            <path
              d={lossPath}
              fill="none"
              stroke={LOSS_COLOR}
              strokeWidth={1.6}
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          )}

          {/* Right edge marker — current value dot for ping */}
          {latestPing != null && (
            <circle
              cx={WIDTH - PADDING_X}
              cy={
                PADDING_TOP +
                (1 - Math.min(1, Math.max(0, latestPing / pingMax))) *
                  (HEIGHT - PADDING_TOP - PADDING_BOTTOM)
              }
              r={2.5}
              fill={PING_COLOR}
            />
          )}
        </svg>
      </div>

      {samples.length === 0 && (
        <div className="px-3 pb-3 text-center text-[11px] text-text-muted">
          Collecting samples…
        </div>
      )}
      {samples.length > 0 && avgLoss > 0.5 && (
        <div className="px-3 pb-3 text-[11px] text-text-muted">
          Avg loss {formatLoss(avgLoss)} over window — sustained loss above 1 %
          can cause stuttering.
        </div>
      )}
    </div>
  );
}
