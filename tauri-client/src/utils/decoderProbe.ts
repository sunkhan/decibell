// Decoder capability probe.
//
// Two paths:
//   - Webview HAS WebCodecs (Windows, web): call
//     VideoDecoder.isConfigSupported per codec — the webview decodes
//     natively, so this is the authoritative answer.
//   - Webview LACKS WebCodecs (Linux WebKitGTK): invoke the Rust-side
//     probe via Tauri (`probe_decoders_native`). Linux decodes H.264 /
//     HEVC / AV1 in Rust through ffmpeg with NVDEC / VAAPI / software
//     fallback; the Rust probe walks each codec and reports what
//     actually opens. Without this, the LCD picker would always
//     downgrade Linux watchers to H.264 even if their GPU could
//     happily decode AV1.
//
// Result cached in localStorage so we don't re-probe on every app launch
// — Settings → "Refresh codec capabilities" forces a re-probe.
//
// H.264 decode is always present in the result regardless of probe outcome
// (spec §3.3 fallback) so the LCD picker always has a converging codec.

import { invoke } from "@tauri-apps/api/core";
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

  let out: CodecCapability[] = [];

  if (typeof VideoDecoder !== "undefined") {
    // Path A — webview decodes natively. Probe each codec via WebCodecs.
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
        out.push({ codec, maxWidth: ceiling.width, maxHeight: ceiling.height, maxFps: ceiling.fps });
      }
    }
  } else {
    // Path B — webview can't decode (Linux WebKitGTK). Ask Rust what
    // ffmpeg + NVDEC / VAAPI / software can actually open. Rust returns
    // the same CodecCapability shape (codec / max_width / max_height /
    // max_fps) so downstream code (set_decoder_caps, JoinVoiceRequest)
    // works identically regardless of which path filled it.
    try {
      const native = await invoke<CodecCapability[]>("probe_decoders_native");
      if (Array.isArray(native)) {
        out = native;
      }
    } catch (e) {
      console.error("[decoderProbe] probe_decoders_native failed:", e);
    }
  }

  // §3.3 fallback: H.264 decode is always advertised so the LCD picker
  // always has a converging codec. ffmpeg ships H.264 software decode
  // built in, so the Rust probe should already cover this — this is a
  // belt-and-braces guarantee.
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
