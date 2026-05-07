// Decoder capability probe.
//
// Chromium/Electron always exposes WebCodecs, so this is the
// single-path version: ask `VideoDecoder.isConfigSupported` for each
// candidate codec and report what comes back. Result is cached in
// localStorage so we don't re-probe on every app launch — Settings →
// "Refresh codec capabilities" forces a re-probe.
//
// H.264 decode is always present in the result regardless of probe
// outcome (spec §3.3 fallback) so the LCD picker always has a
// converging codec.

import { VideoCodec, type CodecCapability } from "../types";

// Decode policy ceilings — spec §3.2. Match encoder ceilings in
// caps.rs:encode_ceiling so codec selection stops downgrading 120 fps
// streamers when a viewer joins.
const DECODE_CEILING: Record<number, { width: number; height: number; fps: number }> = {
  [VideoCodec.AV1]: { width: 3840, height: 2160, fps: 120 },
  [VideoCodec.H265]: { width: 3840, height: 2160, fps: 120 },
  [VideoCodec.H264_HW]: { width: 3840, height: 2160, fps: 120 },
};

// Conservative "well-known" codec strings. Match codecMap.ts so a
// supported-during-probe claim translates 1:1 to actual decode at runtime.
const PROBE_CONFIGS: { codec: VideoCodec; webCodecsString: string }[] = [
  { codec: VideoCodec.AV1, webCodecsString: "av01.0.19M.08" },
  { codec: VideoCodec.H265, webCodecsString: "hvc1.1.6.L186.B0" },
  { codec: VideoCodec.H264_HW, webCodecsString: "avc1.64003E" },
];

const LOCAL_STORAGE_KEY = "decibell.decoder_caps.v2";

export async function probeDecoders(force = false): Promise<CodecCapability[]> {
  if (!force) {
    const cached = localStorage.getItem(LOCAL_STORAGE_KEY);
    if (cached) {
      try {
        const parsed: CodecCapability[] = JSON.parse(cached);
        if (Array.isArray(parsed) && parsed.length > 0) return parsed;
      } catch {
        /* fall through to fresh probe */
      }
    }
  }

  const out: CodecCapability[] = [];

  if (typeof VideoDecoder !== "undefined") {
    for (const { codec, webCodecsString } of PROBE_CONFIGS) {
      let supported = false;
      try {
        const res = await VideoDecoder.isConfigSupported({
          codec: webCodecsString,
          hardwareAcceleration: "prefer-hardware",
        });
        supported = !!res.supported;
      } catch {
        supported = false;
      }
      if (supported) {
        const ceiling = DECODE_CEILING[codec];
        out.push({
          codec,
          maxWidth: ceiling.width,
          maxHeight: ceiling.height,
          maxFps: ceiling.fps,
        });
      }
    }
  }

  // §3.3 fallback: H.264 decode is always advertised so the LCD picker
  // always has a converging codec.
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
