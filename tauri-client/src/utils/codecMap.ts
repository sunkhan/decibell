// VideoCodec → WebCodecs codec string mapping for VideoDecoder.configure().
//
// The strings here are conservative "well-known" profile/level codes; real
// streams may use different parameters but the description record (avcC /
// hvcC / av1C) supplied alongside fully describes the stream and the decoder
// accepts any compatible variant once configured.

import { VideoCodec } from "../types";

export function videoCodecToWebCodecsString(c: VideoCodec): string {
  switch (c) {
    case VideoCodec.AV1:    return "av01.0.05M.08";    // Main, level 5.1, 8-bit
    case VideoCodec.H265:   return "hev1.1.6.L120.B0"; // Main, level 4.0
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW: return "avc1.640033";    // High, level 5.1
    default: return "avc1.640033"; // safe fallback for UNKNOWN
  }
}

/// Human-readable codec name for UI labels (badge, tooltips, error messages).
export function videoCodecHumanName(c: VideoCodec): string {
  switch (c) {
    case VideoCodec.AV1:    return "AV1";
    case VideoCodec.H265:   return "H.265";
    case VideoCodec.H264_HW: return "H.264";
    case VideoCodec.H264_SW: return "H.264 (software)";
    default: return "Unknown";
  }
}
