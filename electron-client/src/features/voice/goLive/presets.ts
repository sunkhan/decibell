import type { useVoiceStore } from "../../../stores/voiceStore";
import type { IconName } from "../../servers/tabs/settingsUi";

/// Stream quality presets: named combinations of the stored settings. A
/// preset is never stored itself; the dialog shows the one the current
/// resolution, frame rate and quality step match, or "custom".

type StreamSettings = ReturnType<typeof useVoiceStore.getState>["streamSettings"];
export type StreamResolution = StreamSettings["resolution"];
type QualityStep = Exclude<StreamSettings["quality"], "custom">;

/// Video bitrate (kbps) of each quality step. "Source" gets the larger
/// column: it is announced as 1440p (see resolveDimensions).
export function stepBitrate(resolution: StreamResolution, quality: QualityStep): number {
  const big = resolution === "source";
  switch (quality) {
    case "low":
      return big ? 6000 : 3000;
    case "medium":
      return big ? 12000 : 6000;
    default:
      return big ? 20000 : 10000;
  }
}

export interface StreamPreset {
  id: "smooth" | "sharp" | "slides" | "saver";
  label: string;
  icon: IconName;
  /// The card's second line.
  spec: string;
  /// The sentence under the cards while this preset is picked.
  blurb: string;
  resolution: StreamResolution;
  fps: StreamSettings["fps"];
  quality: QualityStep;
}

export const STREAM_PRESETS: StreamPreset[] = [
  {
    id: "smooth",
    label: "Smooth",
    icon: "gamepad",
    spec: "1080p · 60 fps",
    blurb: "Fluid motion at 1080p and 60 fps. Best for games and video.",
    resolution: "1080p",
    fps: 60,
    quality: "high",
  },
  {
    id: "sharp",
    label: "Sharp",
    icon: "text",
    spec: "Full res · 30 fps",
    blurb: "Your screen's full resolution at 30 fps, so text and code stay crisp.",
    resolution: "source",
    fps: 30,
    quality: "medium",
  },
  // Its 5 fps also tunes the native encoder for detail (start_screen_share's
  // `detail`): the quality preset, a keyframe a minute, 4K for H.264. That
  // follows the frame rate, so a Slides stream adjusted under Advanced keeps
  // it; streams at any other frame rate are encoded as before.
  {
    id: "slides",
    label: "Slides",
    icon: "presentation",
    spec: "Full res · 5 fps",
    blurb: "Your screen's full resolution at 5 fps, with every frame encoded for detail. Best for presentations.",
    resolution: "source",
    fps: 5,
    quality: "medium",
  },
  {
    id: "saver",
    label: "Data saver",
    icon: "gauge",
    spec: "720p · 30 fps",
    blurb: "720p at 30 fps, a third of Smooth's bandwidth. For slow connections.",
    resolution: "720p",
    fps: 30,
    quality: "low",
  },
];

export function matchPreset(s: StreamSettings): StreamPreset | null {
  return (
    STREAM_PRESETS.find(
      (p) => p.resolution === s.resolution && p.fps === s.fps && p.quality === s.quality,
    ) ?? null
  );
}

export function presetSettings(p: StreamPreset): Partial<StreamSettings> {
  return {
    resolution: p.resolution,
    fps: p.fps,
    quality: p.quality,
    videoBitrateKbps: stepBitrate(p.resolution, p.quality),
  };
}

export function resolutionLabel(r: StreamResolution): string {
  return r === "source" ? "Source" : r;
}

export function formatBitrate(kbps: number): string {
  if (kbps < 1000) return `${kbps} kbps`;
  const mbps = kbps / 1000;
  return `${mbps.toFixed(kbps % 1000 === 0 ? 0 : 1)} Mbps`;
}
