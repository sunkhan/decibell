// VideoCodec → WebCodecs codec string mapping for VideoDecoder.configure().
//
// The strings here are conservative "well-known" profile/level codes; real
// streams may use different parameters but the description record (avcC /
// hvcC / av1C) supplied alongside fully describes the stream and the decoder
// accepts any compatible variant once configured.

import { VideoCodec } from "../types";

export function videoCodecToWebCodecsString(c: VideoCodec): string {
  switch (c) {
    // Max defined level for each codec — the string is a capability ceiling
    // ("decoder, accept anything up to this"); the actual stream description
    // (av1C / hvcC / avcC) is what Chromium uses to do the work. Higher
    // ceiling = no risk of artificial gating when we bump encode caps later.
    case VideoCodec.AV1:    return "av01.0.19M.08";    // Main, level 6.3, 8-bit (covers up to 16K@120)
    // hvc1 (not hev1) — parameter sets live ONLY in the description record
    // because the Rust encoder sets AV_CODEC_FLAG_GLOBAL_HEADER for HEVC.
    case VideoCodec.H265:   return "hvc1.1.6.L186.B0"; // Main, level 6.2 (covers up to 8K@120)
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW: return "avc1.64003E";    // High, level 6.2 (covers up to 8K@60)
    default: return "avc1.64003E"; // safe fallback for UNKNOWN
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
