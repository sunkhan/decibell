// Pill badge rendered in the top-right of a stream tile.
// Format: "1080p60 · AV1" with a lock icon when the streamer has
// enforced this codec.

import { VideoCodec } from "../../types";
import { videoCodecHumanName } from "../../utils/codecMap";

interface Props {
  codec: VideoCodec;
  width: number;
  height: number;
  fps: number;
  enforced: boolean;
  /// "small" for stream cards, "large" for fullscreen player.
  size?: "small" | "large";
}

const CODEC_COLOR: Record<number, string> = {
  [VideoCodec.AV1]:    "#A78BFA", // purple
  [VideoCodec.H265]:   "#60A5FA", // blue
  [VideoCodec.H264_HW]: "#22D3EE", // teal
  [VideoCodec.H264_SW]: "#94A3B8", // gray
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
    return null; // server hasn't broadcast info yet
  }
  const color = CODEC_COLOR[codec] ?? "#94A3B8";
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
        borderRadius: 999,
        background: "rgba(0,0,0,0.62)",
        color: "white",
        fontSize,
        fontWeight: 600,
        backdropFilter: "blur(6px)",
        zIndex: 5,
        pointerEvents: "none",
      }}
      title={enforced ? `Stream locked to ${label}` : undefined}
    >
      {(width > 0 && height > 0 && fps > 0) && (
        <span style={{ color: "#cbd5e1" }}>{formatResolution(width, height)}{fps}</span>
      )}
      <span style={{ color }}>{label}</span>
      {enforced && (
        <svg width={fontSize} height={fontSize} viewBox="0 0 24 24" fill="none" stroke="#FBBF24" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
          <rect x="4" y="11" width="16" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 018 0v4" />
        </svg>
      )}
    </div>
  );
}
