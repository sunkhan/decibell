import { VideoCodec } from "../../types";
import { videoCodecHumanName } from "../../utils/codecMap";

interface Props {
  codec: VideoCodec;
  width: number;
  height: number;
  fps: number;
  enforced: boolean;
  size?: "small" | "large";
}

// Per-codec accent. Tokens where the palette has an equivalent; H.264 HW
// keeps a raw teal because there's no matching design-system color (it
// sits between accent-bright and success).
const CODEC_COLOR: Record<number, string> = {
  [VideoCodec.AV1]:     "var(--color-success)",
  [VideoCodec.H265]:    "var(--color-accent-bright)",
  [VideoCodec.H264_HW]: "#22D3EE",
  [VideoCodec.H264_SW]: "var(--color-text-secondary)",
};

function formatResolution(w: number, h: number): string {
  if (w === 3840 && h === 2160) return "4K";
  if (w === 2560 && h === 1440) return "1440p";
  if (w === 1920 && h === 1080) return "1080p";
  if (w === 1280 && h === 720)  return "720p";
  return `${w}×${h}`;
}

export function CodecBadge({ codec, width, height, fps, enforced, size = "small" }: Props) {
  if (codec === VideoCodec.UNKNOWN && width === 0 && height === 0 && fps === 0) {
    return null;
  }
  const color = CODEC_COLOR[codec] ?? "var(--color-text-secondary)";
  const label = videoCodecHumanName(codec);
  const fontSize = size === "large" ? 13 : 10.5;
  const padX = size === "large" ? 12 : 8;
  const padY = size === "large" ? 6 : 3.5;
  const gap = size === "large" ? 8 : 5;

  return (
    <div
      style={{
        position: "absolute",
        top: 8,
        right: 8,
        display: "inline-flex",
        alignItems: "center",
        gap,
        padding: `${padY}px ${padX}px`,
        borderRadius: 8,
        // Slightly more opaque than --color-bg-darkest alone so the badge
        // stays legible over busy stream content.
        background: "color-mix(in oklab, var(--color-bg-darkest) 75%, transparent)",
        border: "1px solid var(--color-border)",
        color: "white",
        fontSize,
        fontWeight: 600,
        fontFamily: "var(--font-channel)",
        zIndex: 5,
        pointerEvents: "none",
      }}
      title={enforced ? `Stream locked to ${label}` : undefined}
    >
      {(width > 0 && height > 0 && fps > 0) && (
        <span style={{ color: "var(--color-text-secondary)" }}>{formatResolution(width, height)}{fps}</span>
      )}
      <span style={{ color }}>{label}</span>
      {enforced && (
        <svg width={fontSize} height={fontSize} viewBox="0 0 24 24" fill="none" stroke="var(--color-warning)" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
          <rect x="4" y="11" width="16" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 018 0v4" />
        </svg>
      )}
    </div>
  );
}
