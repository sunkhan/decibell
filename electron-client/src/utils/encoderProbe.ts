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
import { probeDecoders } from "./decoderProbe";

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

// v5 → v6: probe now also requires the response's
// `config.hardwareAcceleration` to be 'prefer-hardware' before
// claiming the codec is HW-accelerated. Pre-v6 caches over-reported
// HW on Windows when Chromium silently downgraded the hint while
// still answering `supported: true`.
const LOCAL_STORAGE_KEY = "decibell.encoder_caps.v6";

// Linux native-encode opt-in. Default on so NVIDIA users get NVENC out of
// the box; flip to "0" (e.g. via Settings, or devtools for A/B testing the
// native vs WebCodecs path) to force the renderer WebCodecs path.
const LINUX_NATIVE_KEY = "decibell.linux_native_encode";
function linuxNativeEncodeEnabled(): boolean {
  return localStorage.getItem(LINUX_NATIVE_KEY) !== "0";
}

// Set by probeEncoders(): true when the active encode path is the native
// FFmpeg pipeline (always on Windows; on Linux only when enabled AND a
// hardware encoder was actually probed). StreamCapture reads this to
// decide whether to skip WebCodecs and drive start_screen_share natively.
let nativeEncodeActive = false;

/// Whether the native FFmpeg encode pipeline owns this session (vs the
/// renderer's WebCodecs path). Reflects the most recent probeEncoders().
export function isNativeEncodeActive(): boolean {
  return nativeEncodeActive;
}

export async function probeEncoders(force = false): Promise<CodecCapability[]> {
  const platform = typeof window !== "undefined" ? window.decibell?.platform : undefined;

  // Windows: always native. Chromium's WebCodecs encoder factory caps at
  // 30 fps in this Castlabs build, so its `isConfigSupported` results are
  // misleading (claims HW support at 720p30 but won't allocate 1080p60).
  // Native FFmpeg talks directly to NVENC/AMF/QSV and reports the truth.
  if (platform === "win32") {
    nativeEncodeActive = true;
    return await probeNativeEncoders(force);
  }

  // Linux: native is opt-in and only taken if a hardware encoder is
  // actually present (Chromium WebCodecs doesn't reach NVENC on Linux —
  // that's the whole reason for the native path). If the native probe
  // finds no HW encoder, fall through to the WebCodecs probe so AMD/Intel
  // (where WebCodecs VAAPI works) and GPU-less boxes still stream.
  if (platform === "linux" && linuxNativeEncodeEnabled()) {
    const native = await probeNativeEncoders(force);
    if (native.some((c) => c.hardware)) {
      nativeEncodeActive = true;
      return native;
    }
    console.log("[encoderProbe] no native HW encoder on Linux — using WebCodecs");
  }
  nativeEncodeActive = false;

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
          // prefer-hardware. Two signals together tell us whether HW
          // is actually viable:
          //
          //   - `supported: true/false` (per gotcha 5.2 isConfigSupported
          //     treats the hint as a hard constraint — but only on some
          //     platforms; Chromium on Windows is known to soft-drop)
          //   - `response.config.hardwareAcceleration` — the
          //     hint Chromium actually committed to. If we asked for
          //     prefer-hardware and got back 'no-preference' or
          //     'prefer-software', the hint was silently downgraded
          //     and HW is NOT actually available.
          //
          // For H264_SW we always claim hardware: false since the user
          // picked software explicitly.
          let hardware = false;
          let negotiated: string | undefined;
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
              negotiated = hwSupport.config?.hardwareAcceleration;
              hardware =
                !!hwSupport.supported && negotiated === "prefer-hardware";
            } catch {
              hardware = false;
            }
          }
          console.log(
            `[encoderProbe] codec=${cfg.codec} via ` +
              `${support.config?.codec ?? cfg.webCodecsString} ` +
              `(${hardware ? "HW" : "SW"}` +
              (negotiated ? `, negotiated=${negotiated}` : "") +
              `)`,
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

/// Windows-only path: native FFmpeg probe via the napi addon. Native
/// is the source of truth on Windows — Chromium's WebCodecs encoder
/// factory caps at 30 fps and lies about supported configurations.
/// The cache key is scoped separately so a Linux→Windows dual-boot
/// dev environment doesn't accidentally show stale WebCodecs caps.
///
/// Caps are also shipped to native's AppState.encoder_caps via
/// set_encoder_caps so subsequent get_caps reads (e.g. when the
/// Settings → Codecs page mounts and the store calls load()) return
/// the populated list. Without this, opening Settings while a stream
/// is running would overwrite the in-memory caps with an empty list
/// and the codec picker would collapse back to "Auto" only.
async function probeNativeEncoders(
  force: boolean,
): Promise<CodecCapability[]> {
  const NATIVE_KEY = "decibell.native_encoder_caps.v1";
  let caps: CodecCapability[] | null = null;
  if (!force) {
    const cached = localStorage.getItem(NATIVE_KEY);
    if (cached) {
      try {
        const parsed: CodecCapability[] = JSON.parse(cached);
        if (Array.isArray(parsed) && parsed.length > 0) {
          console.log(
            `[encoderProbe/native] using cached caps (${parsed.length} codecs)`,
          );
          caps = parsed;
        }
      } catch {
        /* fall through and re-probe */
      }
    }
  }
  if (caps === null) {
    try {
      type NativeCap = {
        codec: number;
        maxWidth: number;
        maxHeight: number;
        maxFps: number;
        hardware: boolean;
        encoderName: string;
      };
      const raw = (await invoke("probe_native_encoders", {})) as NativeCap[];
      caps = raw.map((c) => ({
        codec: c.codec as VideoCodec,
        maxWidth: c.maxWidth,
        maxHeight: c.maxHeight,
        maxFps: c.maxFps,
        hardware: c.hardware,
      }));
      for (const c of raw) {
        console.log(
          `[encoderProbe/native] codec=${c.codec} via ${c.encoderName} (${
            c.hardware ? "HW" : "SW"
          })`,
        );
      }
      localStorage.setItem(NATIVE_KEY, JSON.stringify(caps));
    } catch (e) {
      console.error("[encoderProbe/native] probe failed:", e);
      return [];
    }
  }

  // Only offer codecs the local Chromium can also DECODE. The native
  // encoder can produce HEVC (NVENC), but Chromium on Linux can't decode
  // it — so the streamer's own self-preview (and same-platform watchers)
  // would get a codec they can't render (infinite spinner). Intersecting
  // with the decoder probe drops HEVC on Linux while keeping H.264 + AV1
  // (AV1 supersedes HEVC anyway); on Windows, where HEVC decode exists,
  // nothing is dropped.
  try {
    const decodable = new Set((await probeDecoders()).map((c) => c.codec));
    const usable = caps.filter((c) => decodable.has(c.codec));
    if (usable.length < caps.length) {
      const dropped = caps
        .filter((c) => !decodable.has(c.codec))
        .map((c) => c.codec);
      console.log(
        `[encoderProbe/native] dropping encode codec(s) not locally decodable: ${dropped.join(", ")}`,
      );
      caps = usable;
    }
  } catch (e) {
    console.warn("[encoderProbe/native] decode-intersect skipped:", e);
  }
  // Ship to native's encoder_caps slot every boot — both fresh probes
  // and cache-hit code paths. AppState.encoder_caps is in-memory and
  // resets on app launch; we re-populate it from cache so the codec
  // dropdown survives a Settings → load() roundtrip without re-probing.
  invoke("set_encoder_caps", { encoderCaps: caps }).catch((e) =>
    console.warn("[encoderProbe/native] set_encoder_caps failed:", e),
  );
  return caps;
}
