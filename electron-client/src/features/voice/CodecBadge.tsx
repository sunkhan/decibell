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

// Per-codec accent. Deliberately theme-independent: the badge floats
// over decoded video, not over a themed surface, so it keeps a dark
// chip with light type in every palette — routing it through the DS
// tokens would put dark-on-dark type over the light themes' video.
const CODEC_COLOR: Record<number, string> = {
  [VideoCodec.AV1]: "#4ade80",
  [VideoCodec.H265]: "#93c5fd",
  [VideoCodec.H264_HW]: "#22d3ee",
  [VideoCodec.H264_SW]: "rgba(255,255,255,0.70)",
};

function formatResolution(w: number, h: number): string {
  if (w === 3840 && h === 2160) return "4K";
  if (w === 2560 && h === 1440) return "1440p";
  if (w === 1920 && h === 1080) return "1080p";
  if (w === 1280 && h === 720) return "720p";
  return `${w}×${h}`;
}

export function CodecBadge({
  codec,
  width,
  height,
  fps,
  enforced,
  size = "small",
}: Props) {
  if (codec === VideoCodec.UNKNOWN && width === 0 && height === 0 && fps === 0) {
    return null;
  }
  const color = CODEC_COLOR[codec] ?? "rgba(255,255,255,0.70)";
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
        background: "rgba(0,0,0,0.72)",
        border: "1px solid rgba(255,255,255,0.14)",
        color: "white",
        fontSize,
        fontWeight: 600,
        fontFamily: "var(--font-channel)",
        zIndex: 5,
        pointerEvents: "none",
      }}
      title={enforced ? `Stream locked to ${label}` : undefined}
    >
      {width > 0 && height > 0 && fps > 0 && (
        <span style={{ color: "rgba(255,255,255,0.70)" }}>
          {formatResolution(width, height)}
          {fps}
        </span>
      )}
      <span style={{ color }}>{label}</span>
      {enforced && (
        <svg
          width={fontSize}
          height={fontSize}
          viewBox="0 0 24 24"
          fill="none"
          stroke="#e0b050"
          strokeWidth="2.4"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <rect x="4" y="11" width="16" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 018 0v4" />
        </svg>
      )}
    </div>
  );
}
