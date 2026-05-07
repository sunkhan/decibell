import { useEffect, useMemo, useRef } from "react";
import { useVoiceStatsStore, type ConnectionStatsSample } from "../../stores/voiceStatsStore";

interface Props {
  onClose: () => void;
}

const WIDTH = 244;
const HEIGHT = 120;
const PADDING_X = 8;
const PADDING_TOP = 12;
const PADDING_BOTTOM = 16;
const WINDOW_MS = 5 * 60 * 1000;

// CSS-var strings rather than raw hex so theme changes propagate
// without recompiling. SVG `stroke` / `fill` / `stopColor` all accept
// `var(...)` in WebKit and Chromium webviews.
const PING_COLOR = "var(--color-accent)";
const LOSS_COLOR = "var(--color-error)";

function formatPing(p: number | null): string {
  return p == null ? "—" : `${p} ms`;
}
function formatLoss(p: number): string {
  return p < 0.1 ? "0%" : p < 10 ? `${p.toFixed(1)}%` : `${Math.round(p)}%`;
}

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
    const tNorm = 1 - (now - s.ts) / WINDOW_MS;
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

/** Build a closed polygon path for the gradient fill under the ping line. */
function buildFillPath(
  samples: ConnectionStatsSample[],
  now: number,
  extract: (s: ConnectionStatsSample) => number | null,
  yMax: number,
): string {
  const w = WIDTH - PADDING_X * 2;
  const h = HEIGHT - PADDING_TOP - PADDING_BOTTOM;
  const bottom = PADDING_TOP + h;
  const points: { x: number; y: number }[] = [];

  for (const s of samples) {
    const v = extract(s);
    if (v == null) continue;
    const tNorm = 1 - (now - s.ts) / WINDOW_MS;
    if (tNorm < 0 || tNorm > 1) continue;
    const x = PADDING_X + tNorm * w;
    const yNorm = Math.min(1, Math.max(0, v / yMax));
    const y = PADDING_TOP + (1 - yNorm) * h;
    points.push({ x, y });
  }

  if (points.length < 2) return "";

  let d = `M ${points[0].x.toFixed(1)} ${bottom.toFixed(1)}`;
  for (const p of points) {
    d += ` L ${p.x.toFixed(1)} ${p.y.toFixed(1)}`;
  }
  d += ` L ${points[points.length - 1].x.toFixed(1)} ${bottom.toFixed(1)} Z`;
  return d;
}

export default function ConnectionStatsPopover({ onClose }: Props) {
  const samples = useVoiceStatsStore((s) => s.samples);
  const popoverRef = useRef<HTMLDivElement | null>(null);

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

  const now = useMemo(() => Date.now(), [samples]);

  const pingMax = useMemo(() => {
    let max = 100;
    for (const s of samples) {
      if (s.pingMs != null && s.pingMs > max) max = s.pingMs;
    }
    return Math.ceil(max / 50) * 50;
  }, [samples]);

  const pingPath = useMemo(
    () => buildPath(samples, now, (s) => s.pingMs, pingMax),
    [samples, now, pingMax],
  );
  const pingFillPath = useMemo(
    () => buildFillPath(samples, now, (s) => s.pingMs, pingMax),
    [samples, now, pingMax],
  );
  const lossPath = useMemo(
    () => buildPath(samples, now, (s) => s.lossPct, 100),
    [samples, now],
  );

  const latest = samples.length > 0 ? samples[samples.length - 1] : null;
  const latestPing = latest?.pingMs ?? null;
  const latestLoss = latest?.lossPct ?? 0;

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
      className="absolute bottom-[calc(100%+6px)] left-0 z-50 w-[260px] overflow-hidden rounded-xl border border-border bg-bg-light shadow-[0_8px_32px_rgba(0,0,0,0.45),0_0_0_1px_rgba(255,255,255,0.02)]"
      onClick={(e) => e.stopPropagation()}
    >
      {/* Header */}
      <div className="border-b border-border-divider px-4 py-3">
        <div className="mb-2.5 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
          Connection
        </div>
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-1.5">
            <span className="h-[7px] w-[7px] rounded-full" style={{ background: PING_COLOR }} />
            <span className="text-[12px] text-text-secondary">Ping</span>
            <span className="whitespace-nowrap text-[12.5px] font-semibold tabular-nums text-text-primary">
              {formatPing(latestPing)}
            </span>
            {avgPing != null && avgPing !== latestPing && (
              <span className="whitespace-nowrap text-[11px] tabular-nums text-text-muted">
                (avg {avgPing} ms)
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-[7px] w-[7px] rounded-full" style={{ background: LOSS_COLOR }} />
            <span className="text-[12px] text-text-secondary">Loss</span>
            <span className="whitespace-nowrap text-[12.5px] font-semibold tabular-nums text-text-primary">
              {formatLoss(latestLoss)}
            </span>
          </div>
        </div>
      </div>

      {/* Chart */}
      <div className="px-4 pb-3 pt-2.5">
        <div className="relative overflow-hidden rounded-[10px] border border-border-divider bg-bg-mid">
          <svg
            width={WIDTH}
            height={HEIGHT}
            viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
            className="block"
          >
            <defs>
              <linearGradient id="pingGradFill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor={PING_COLOR} stopOpacity="0.15" />
                <stop offset="100%" stopColor={PING_COLOR} stopOpacity="0" />
              </linearGradient>
            </defs>

            {/* Horizontal grid lines */}
            {[0, 0.5, 1].map((frac) => {
              const y = PADDING_TOP + (HEIGHT - PADDING_TOP - PADDING_BOTTOM) * frac;
              return (
                <line
                  key={frac}
                  x1={PADDING_X}
                  x2={WIDTH - PADDING_X}
                  y1={y}
                  y2={y}
                  stroke="rgba(255,255,255,0.03)"
                  strokeWidth={0.5}
                />
              );
            })}

            {/* Y-axis labels */}
            <text
              x={PADDING_X + 2}
              y={PADDING_TOP + 8}
              fill="var(--color-text-faint)"
              fontSize="10"
              fontFamily="inherit"
            >
              {pingMax} ms
            </text>
            <text
              x={PADDING_X + 2}
              y={PADDING_TOP + (HEIGHT - PADDING_TOP - PADDING_BOTTOM) * 0.5 + 3}
              fill="var(--color-text-faint)"
              fontSize="10"
              fontFamily="inherit"
            >
              {Math.round(pingMax / 2)} ms
            </text>
            <text
              x={PADDING_X + 2}
              y={HEIGHT - PADDING_BOTTOM - 2}
              fill="var(--color-text-faint)"
              fontSize="10"
              fontFamily="inherit"
            >
              0 ms
            </text>

            {/* X-axis label */}
            <text
              x={WIDTH - PADDING_X - 2}
              y={HEIGHT - 3}
              textAnchor="end"
              fill="var(--color-text-faint)"
              fontSize="10"
              fontFamily="inherit"
            >
              last 5 min
            </text>

            {/* Gradient fill under ping line */}
            {pingFillPath && (
              <path
                d={pingFillPath}
                fill="url(#pingGradFill)"
              />
            )}

            {/* Ping line */}
            {pingPath && (
              <path
                d={pingPath}
                fill="none"
                stroke={PING_COLOR}
                strokeWidth={1.5}
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            )}

            {/* Loss line */}
            {lossPath && (
              <path
                d={lossPath}
                fill="none"
                stroke={LOSS_COLOR}
                strokeWidth={1.5}
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeDasharray="3 3"
              />
            )}

            {/* Current value dot for ping */}
            {latestPing != null && (
              <>
                <circle
                  cx={WIDTH - PADDING_X}
                  cy={
                    PADDING_TOP +
                    (1 - Math.min(1, Math.max(0, latestPing / pingMax))) *
                      (HEIGHT - PADDING_TOP - PADDING_BOTTOM)
                  }
                  r={4}
                  fill={PING_COLOR}
                  opacity={0.2}
                />
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
              </>
            )}
          </svg>
        </div>
      </div>

      {samples.length === 0 && (
        <div className="px-4 pb-3 text-center text-[11px] text-text-muted">
          Collecting samples…
        </div>
      )}
      {samples.length > 0 && avgLoss > 0.5 && (
        <div className="px-4 pb-3 text-[11px] leading-relaxed text-text-muted">
          Avg loss {formatLoss(avgLoss)} over window — sustained loss above 1%
          can cause stuttering.
        </div>
      )}
    </div>
  );
}
