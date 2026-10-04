/// The Decibell mark: level bars traced along a bell curve — the app
/// icon's drawing, in the theme's accent. Bars brighten toward the
/// centre the way the icon's do. `rise` plays the one-shot entrance
/// (bars swell from the centre outward, like a struck bell) and is
/// skipped under prefers-reduced-motion by the keyframe's own guard.
export function BellMark({
  bars = 11,
  height = 28,
  barWidth = 3,
  gap = 2,
  rise = false,
  barClass = "bg-accent",
  className = "",
}: {
  bars?: number;
  height?: number;
  barWidth?: number;
  gap?: number;
  rise?: boolean;
  barClass?: string;
  className?: string;
}) {
  const centre = (bars - 1) / 2;
  // σ scales with the bar count so every size keeps the icon's shape:
  // the outermost bars land near 15% of the peak.
  const sigma = centre / 1.95;
  return (
    <div
      aria-hidden
      className={`flex items-end ${className}`}
      style={{ height, gap }}
    >
      {Array.from({ length: bars }, (_, i) => {
        const d = Math.abs(i - centre);
        const level = Math.exp(-(d * d) / (2 * sigma * sigma));
        return (
          <div
            key={i}
            className={`origin-bottom rounded-full ${barClass} ${rise ? "bell-rise" : ""}`}
            style={{
              width: barWidth,
              height: Math.max(barWidth, Math.round(height * level)),
              opacity: 0.4 + 0.6 * level,
              animationDelay: rise ? `${Math.round(d * 55)}ms` : undefined,
            }}
          />
        );
      })}
    </div>
  );
}
