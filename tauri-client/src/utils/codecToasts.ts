// Toast text builder for STREAM_CODEC_CHANGED_NOTIFY events.
// Picks streamer-side vs viewer-side wording based on whether the
// local user is the stream owner.

import { VideoCodec, StreamCodecChangeReason, type StreamCodecChangedNotify } from "../types";
import { videoCodecHumanName } from "./codecMap";

function resLabel(w: number, h: number, fps: number): string {
  const r = w === 3840 && h === 2160 ? "4K"
    : w === 2560 && h === 1440 ? "1440p"
    : w === 1920 && h === 1080 ? "1080p"
    : w === 1280 && h === 720  ? "720p"
    : `${w}×${h}`;
  return `${r}${fps}`;
}

export function buildCodecToast(
  notify: StreamCodecChangedNotify,
  isLocalUserStreamer: boolean,
): { text: string } | null {
  const codec = videoCodecHumanName(notify.newCodec as VideoCodec);
  const res = resLabel(notify.newWidth, notify.newHeight, notify.newFps);

  switch (notify.reason) {
    case StreamCodecChangeReason.WATCHER_JOINED_LOW_CAPS:
      return {
        text: isLocalUserStreamer
          ? `Switched to ${codec} at ${res} so a viewer can watch.`
          : `${notify.streamerUsername} switched to ${codec} (${res}).`,
      };
    case StreamCodecChangeReason.LIMITING_WATCHER_LEFT:
      return {
        text: isLocalUserStreamer
          ? `Restored to ${codec} at ${res}.`
          : `${notify.streamerUsername} restored to ${codec} (${res}).`,
      };
    case StreamCodecChangeReason.STREAMER_INITIATED:
      return {
        text: isLocalUserStreamer
          ? `Codec changed to ${codec} at ${res}.`
          : `${notify.streamerUsername} switched to ${codec} (${res}).`,
      };
    default:
      return null;
  }
}
