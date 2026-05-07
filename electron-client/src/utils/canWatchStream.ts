// Decides whether the local client can subscribe to a given stream.
// Checks both currentCodec (must be in our decode caps) and enforcedCodec
// (when set, also must be in our decode caps — enforcement is the
// streamer's lock that disables auto-renegotiation).
//
// Used for the grayed-out Watch button tooltip + click-blocking.

import { VideoCodec, type CodecCapability, type StreamInfo } from "../types";
import { videoCodecHumanName } from "./codecMap";

export function canWatchStream(
  stream: StreamInfo,
  localDecodeCaps: CodecCapability[],
): { canWatch: boolean; reason?: string } {
  const has = (codec: VideoCodec) =>
    codec === VideoCodec.UNKNOWN || localDecodeCaps.some((c) => c.codec === codec);

  if (stream.enforcedCodec !== VideoCodec.UNKNOWN && !has(stream.enforcedCodec)) {
    return {
      canWatch: false,
      reason: `Cannot decode ${videoCodecHumanName(stream.enforcedCodec)} — streamer has locked this codec.`,
    };
  }
  if (stream.currentCodec !== VideoCodec.UNKNOWN && !has(stream.currentCodec)) {
    return {
      canWatch: false,
      reason: `Cannot decode ${videoCodecHumanName(stream.currentCodec)} — your hardware/browser doesn't support it.`,
    };
  }
  return { canWatch: true };
}
