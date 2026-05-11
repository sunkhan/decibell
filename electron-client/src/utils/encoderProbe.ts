// Encoder capability probe (PR8 — replaces native FFmpeg probe).
//
// PR8 moved video encoding to Chromium's `WebCodecs.VideoEncoder`. We
// ask `VideoEncoder.isConfigSupported` for each candidate codec at
// boot and ship the results to native via `set_encoder_caps` so the
// JoinVoiceRequest's ClientCapabilities advertises what we can encode.
// The codec dropdown in CaptureSourcePicker reads the same list.
//
// HEVC encode is gated behind Electron's PlatformHEVCEncoderSupport
// feature flag we set in main/index.ts; the probe reflects whether
// that flag actually lit up HEVC on the user's hardware.

import { VideoCodec, type CodecCapability } from "../types";
import { invoke } from "../lib/ipc";

// Match decoderProbe ceilings + caps.rs::encode_ceiling so the LCD
// picker doesn't downgrade 120 fps streamers when a viewer joins.
const ENCODE_CEILING: Record<number, { width: number; height: number; fps: number }> = {
  [VideoCodec.AV1]: { width: 3840, height: 2160, fps: 120 },
  [VideoCodec.H265]: { width: 3840, height: 2160, fps: 120 },
  [VideoCodec.H264_HW]: { width: 2560, height: 1440, fps: 120 },
  [VideoCodec.H264_SW]: { width: 1920, height: 1080, fps: 60 },
};

// Probe with a low-bitrate, low-resolution config and ONLY the bare
// minimum required fields. Important: `latencyMode` and
// `hardwareAcceleration` are *hints* per the WebCodecs spec, but
// Chromium's `isConfigSupported` treats them as hard constraints and
// rejects whenever it can't fulfil them — e.g., `prefer-hardware` for
// H.264 on Linux+NVIDIA without nvidia-vaapi-driver returns
// `supported: false` even though OpenH264 software encode works fine.
// We drop those hints from the probe (just answering "does this codec
// exist at all?") and re-add them at configure-time in StreamCapture
// where Chromium will then fall back to software if hardware isn't
// available.
const PROBE_CONFIGS: { codec: VideoCodec; webCodecsString: string }[] = [
  // AV1 Level 4.0 (seq_level_idx_0=8) — matches the codec string the
  // streaming path uses for all our resolutions. Probing the same
  // string we'll actually configure with avoids "probe says yes but
  // configure fails because the actual level_idx is too low for the
  // bitstream" surprises.
  { codec: VideoCodec.AV1, webCodecsString: "av01.0.08M.08" },
  { codec: VideoCodec.H265, webCodecsString: "hvc1.1.6.L93.B0" },
  // High Profile Level 3.1 — covers up to 720p30. Either H.264 entry
  // resolves to the same underlying Chromium encoder family; the HW vs
  // SW distinction is enforced via `hardwareAcceleration` at
  // configure-time.
  { codec: VideoCodec.H264_HW, webCodecsString: "avc1.64001F" },
  { codec: VideoCodec.H264_SW, webCodecsString: "avc1.64001F" },
];

// Bumped v4 → v5 alongside the --use-angle=gl + NVD_BACKEND=direct
// boot tweaks. v4 caches were probed before those flags were in
// place and recorded hardware:false; v5 forces the probe to re-run
// against the now-hopefully-working VAAPI integration.
const LOCAL_STORAGE_KEY = "decibell.encoder_caps.v5";

export async function probeEncoders(force = false): Promise<CodecCapability[]> {
  let caps: CodecCapability[] | null = null;

  if (!force) {
    const cached = localStorage.getItem(LOCAL_STORAGE_KEY);
    if (cached) {
      try {
        const parsed: CodecCapability[] = JSON.parse(cached);
        if (Array.isArray(parsed) && parsed.length > 0) {
          console.log(`[encoderProbe] using cached caps (${parsed.length} codecs)`);
          caps = parsed;
        }
      } catch {
        // fall through and re-probe
      }
    }
  }

  if (caps === null) {
    if (typeof VideoEncoder === "undefined") {
      console.warn("[encoderProbe] WebCodecs VideoEncoder not available");
      return [];
    }

    caps = [];
    for (const cfg of PROBE_CONFIGS) {
      const ceiling = ENCODE_CEILING[cfg.codec];
      if (!ceiling) continue;

      try {
        const support = await VideoEncoder.isConfigSupported({
          codec: cfg.webCodecsString,
          width: 1280,
          height: 720,
          framerate: 30,
          bitrate: 1_000_000,
        });
        if (support.supported) {
          // Second probe: same config plus hardwareAcceleration:
          // prefer-hardware. Per gotcha 5.2 this returns supported:
          // false when no hardware encoder is available, which is
          // exactly the signal we want — yes/no on HW. For H264_SW
          // we always claim hardware: false since the user picked
          // software explicitly.
          let hardware = false;
          if (cfg.codec !== 2 /* H264_SW */) {
            try {
              const hwSupport = await VideoEncoder.isConfigSupported({
                codec: cfg.webCodecsString,
                width: 1280,
                height: 720,
                framerate: 30,
                bitrate: 1_000_000,
                hardwareAcceleration: "prefer-hardware",
              });
              hardware = !!hwSupport.supported;
            } catch {
              hardware = false;
            }
          }
          console.log(
            `[encoderProbe] codec=${cfg.codec} via ` +
              `${support.config?.codec ?? cfg.webCodecsString} ` +
              `(${hardware ? "HW" : "SW"})`,
          );
          caps.push({
            codec: cfg.codec,
            maxWidth: ceiling.width,
            maxHeight: ceiling.height,
            maxFps: ceiling.fps,
            hardware,
          });
        } else {
          console.log(`[encoderProbe] codec=${cfg.codec}: not supported`);
        }
      } catch (e) {
        console.log(`[encoderProbe] codec=${cfg.codec}: probe failed:`, e);
      }
    }

    localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(caps));
  }

  // Native's encoder_caps is in-memory and resets every app start, so
  // ship caps to it on every boot — including when we returned the
  // cached list. Without this, native sees an empty Vec and the codec
  // dropdown / capability advertisement collapses to "auto" only after
  // the first successful probe is cached.
  invoke("set_encoder_caps", { encoderCaps: caps }).catch((e) =>
    console.warn("[encoderProbe] failed to ship encoder caps to native:", e),
  );

  return caps;
}
