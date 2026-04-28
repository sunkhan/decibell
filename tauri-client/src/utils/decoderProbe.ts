// WebCodecs decoder capability probe.
//
// Calls VideoDecoder.isConfigSupported for each codec we care about and
// returns the supported set with per-codec policy ceilings (spec §3.2).
// Result cached in localStorage so we don't re-probe on every app launch
// — Settings → "Refresh codec capabilities" forces a re-probe.
//
// H.264 decode is always present in the result regardless of probe outcome
// (spec §3.3 fallback) — WebCodecs handles H.264 universally; the
// LCD picker can always converge as long as every client has it.

import { VideoCodec, type CodecCapability } from "../types";

// Decode policy ceilings — spec §3.2.
const DECODE_CEILING: Record<number, { width: number; height: number; fps: number }> = {
  [VideoCodec.AV1]:    { width: 3840, height: 2160, fps: 60 },
  [VideoCodec.H265]:   { width: 3840, height: 2160, fps: 60 },
  [VideoCodec.H264_HW]: { width: 3840, height: 2160, fps: 60 },
};

// Conservative "well-known" WebCodecs codec strings used to probe.
// Real streams may use slightly different profile/level codes — once
// configured with the description record (avcC/hvcC/av1C) the decoder
// accepts any compatible variant of the same codec family.
// Match codecMap.ts — same strings used to probe and to configure, so a
// "supported during probe" claim translates 1:1 to actual decode at runtime.
const PROBE_CONFIGS: { codec: VideoCodec; webCodecsString: string }[] = [
  { codec: VideoCodec.AV1,    webCodecsString: "av01.0.19M.08" },         // Main, level 6.3, 8-bit
  { codec: VideoCodec.H265,   webCodecsString: "hvc1.1.6.L186.B0" },      // Main, level 6.2
  { codec: VideoCodec.H264_HW, webCodecsString: "avc1.64003E" },          // High, level 6.2
];

const LOCAL_STORAGE_KEY = "decibell.decoder_caps.v1";

export async function probeDecoders(force = false): Promise<CodecCapability[]> {
  if (!force) {
    const cached = localStorage.getItem(LOCAL_STORAGE_KEY);
    if (cached) {
      try {
        const parsed: CodecCapability[] = JSON.parse(cached);
        if (Array.isArray(parsed) && parsed.length > 0) return parsed;
      } catch { /* fall through to fresh probe */ }
    }
  }

  const out: CodecCapability[] = [];
  // WebCodecs may not exist in some webviews. In that case we still
  // advertise H.264 decode via the §3.3 fallback below.
  const VideoDecoderCtor: typeof VideoDecoder | undefined =
    typeof VideoDecoder !== "undefined" ? VideoDecoder : undefined;

  for (const { codec, webCodecsString } of PROBE_CONFIGS) {
    let supported = false;
    if (VideoDecoderCtor) {
      try {
        const res = await VideoDecoderCtor.isConfigSupported({
          codec: webCodecsString,
          hardwareAcceleration: "prefer-hardware",
        });
        supported = !!res.supported;
      } catch {
        supported = false;
      }
    }
    if (supported) {
      const ceiling = DECODE_CEILING[codec];
      out.push({ codec, maxWidth: ceiling.width, maxHeight: ceiling.height, maxFps: ceiling.fps });
    }
  }

  // §3.3 fallback: H.264 decode is always advertised. WebCodecs handles
  // H.264 universally; if probe failed, worst case the player surfaces
  // a per-stream error, but the LCD picker always has a converging codec.
  if (!out.some((c) => c.codec === VideoCodec.H264_HW)) {
    out.push({
      codec: VideoCodec.H264_HW,
      maxWidth: DECODE_CEILING[VideoCodec.H264_HW].width,
      maxHeight: DECODE_CEILING[VideoCodec.H264_HW].height,
      maxFps: DECODE_CEILING[VideoCodec.H264_HW].fps,
    });
  }

  localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(out));
  return out;
}

export function clearDecoderCache(): void {
  localStorage.removeItem(LOCAL_STORAGE_KEY);
}
