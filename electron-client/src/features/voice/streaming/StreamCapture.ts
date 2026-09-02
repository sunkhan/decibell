// PR8 send-side streaming.
//
// Chromium's `getDisplayMedia` + `WebCodecs.VideoEncoder` pipeline:
//
//   getDisplayMedia → MediaStreamTrack → MediaStreamTrackProcessor →
//     ReadableStream<VideoFrame> → VideoEncoder.encode → encoded chunk →
//     IPC `send_video_frame` to native → packetise + UDP
//
// Hardware acceleration: `hardwareAcceleration: 'prefer-hardware'` in
// the VideoEncoder config — Chromium uses NVENC / VAAPI / AMF / D3D11
// transparently on platforms where they're available. The whole path
// stays GPU-side when capture + encode are both hardware (no readback
// to CPU memory between frames).

import { invoke } from "../../../lib/ipc";
import { VideoCodec, type StreamAudioMode } from "../../../types";
import { toast } from "../../../stores/toastStore";
import { videoCodecHumanName } from "../../../utils/codecMap";
import {
  isNativeEncodeActive,
  markNativeEncodeFailed,
} from "../../../utils/encoderProbe";

/// Frame shape emitted to local self-preview subscribers. Matches the
/// wire `StreamFrame` shape minus `username` — local frames have only
/// one possible source so subscribers don't need to filter.
export interface LocalEncodedFrame {
  codec: VideoCodec;
  keyframe: boolean;
  timestamp: number;
  data: Uint8Array;
  description: Uint8Array | null;
}

type LocalFrameCallback = (frame: LocalEncodedFrame) => void;
const localFrameSubs = new Set<LocalFrameCallback>();

/// Subscribe to encoded frames from the local streamer's encoder
/// directly, without round-tripping through native + UDP + server.
/// Used by StreamVideoPlayer when the user watches their own stream:
/// the frames arrive in the same shape they would via the wire, so the
/// same WebCodecs decoder pipeline drives the canvas. Returns an
/// unsubscribe fn. Safe to call before streaming starts; the subscriber
/// just sits idle until the encoder is producing.
export function subscribeLocalFrames(cb: LocalFrameCallback): () => void {
  localFrameSubs.add(cb);
  return () => {
    localFrameSubs.delete(cb);
  };
}

export interface StreamCaptureOptions {
  /// VideoCodec enum value (1=H264_HW, 2=H264_SW, 3=H265, 4=AV1).
  /// 0 (UNKNOWN) means "let Chromium pick H264_HW + start; the LCD
  /// picker may downgrade later when a low-cap watcher joins".
  codec: VideoCodec;
  width: number;
  height: number;
  fps: number;
  bitrateKbps: number;
  shareAudio: boolean;
  /// Stream-audio Opus bitrate in kbps. Honored on the Windows native
  /// path (passed straight through to start_screen_share). On Linux /
  /// macOS the picker's separate start_screen_share invoke is the
  /// one that carries this — set both sites to the same value from
  /// the user's stream settings so there's no skew.
  audioBitrateKbps: number;
  /// Share-audio app filter (mode + ticked app identities) so the native
  /// capture starts with the right rule instead of correcting itself a
  /// moment later. Omitted → native uses the filter it last stored.
  audioMode?: StreamAudioMode;
  audioApps?: string[];
  /// Embed the mouse cursor in the captured video (native paths honour
  /// it via the portal/WGC cursor option). Passed through to
  /// start_screen_share.
  includeCursor: boolean;
  /// Routing for the periodic JPEG thumbnail the streamer broadcasts
  /// to non-watching voice-channel participants (so they see a poster
  /// image on the participant tile instead of a black square). The
  /// pump loop draws every Nth frame to an OffscreenCanvas, encodes
  /// it as JPEG, and ships it via `send_stream_thumbnail`. Both absent
  /// during a P2P DM call — there is no community to post thumbnails to
  /// (the peer sees the live frames), so the thumbnail pump is skipped.
  serverId?: string;
  channelId?: string;
  /// When true, getDisplayMedia is requested without width/height
  /// constraints so Chromium delivers the captured surface at its
  /// native resolution (e.g. 2560×1440 on a 1440p monitor). The
  /// encoder is then configured with the negotiated dimensions read
  /// off the track. When false, width/height are passed through as
  /// hard constraints and Chromium scales the surface to match.
  useNativeSize?: boolean;
  /// Pre-picked capture source id from desktopCapturer.getSources (set
  /// by the renderer's custom picker on platforms without a native
  /// Chromium screen-share dialog — Windows in Electron 33). When set,
  /// we bypass `getDisplayMedia` and go through `getUserMedia` with
  /// `chromeMediaSource: 'desktop'` + `chromeMediaSourceId`, which
  /// captures the user's chosen surface deterministically without
  /// triggering setDisplayMediaRequestHandler at all.
  ///
  /// On Linux (no sourceId) we keep the `getDisplayMedia` path so
  /// xdg-desktop-portal handles the picker. On macOS (no sourceId)
  /// the main-process `useSystemPicker: true` hook draws the OS picker.
  sourceId?: string;
  /// Called when the user stops the OS-side capture (closing the
  /// share dialog, ending the browser-share UI, etc.). The picker
  /// uses this to update voiceStore.isStreaming.
  onCaptureEnded?: () => void;
}

export class StreamCapture {
  private encoder: VideoEncoder | null = null;
  private stream: MediaStream | null = null;
  private reader: ReadableStreamDefaultReader<VideoFrame> | null = null;
  private description: Uint8Array | null = null;
  private descriptionLogged = false;
  private codec: VideoCodec;
  private opts: StreamCaptureOptions;
  private wantKeyframe = true;
  private stopping = false;
  /// True once start() took the native (start_screen_share) branch —
  /// Windows always, Linux when a HW encoder was probed. Drives stop() to
  /// tear the native engine down via stop_screen_share regardless of which
  /// caller invoked stop, so no error path can orphan a running encoder.
  private usedNative = false;
  private encoderConfig: VideoEncoderConfig | null = null;
  private buildEncoder: (() => VideoEncoder) | null = null;
  private preferHardwareTried = false;
  // Periodic thumbnail capture state. The streamer-side draws every
  // Nth VideoFrame to an OffscreenCanvas, JPEG-encodes it, and ships
  // it to native via send_stream_thumbnail. Other voice-channel
  // participants who aren't watching the live stream see this as a
  // poster image. The native side used to do this on the FFmpeg path;
  // PR8's Chromium-encoder path moved capture to the renderer too.
  private thumbnailCanvas: OffscreenCanvas | null = null;
  private lastThumbnailAt = 0;
  /// Only allow one in-flight thumbnail JPEG encode + IPC at a time.
  /// convertToBlob is async; without this guard a slow main process
  /// would queue thumbnails forever.
  private thumbnailInFlight = false;
  private static readonly THUMBNAIL_INTERVAL_MS = 3000;
  private static readonly THUMBNAIL_MAX_EDGE = 320;

  /// The capture source id this session was started with, if any.
  get sourceId(): string | undefined {
    return this.opts.sourceId;
  }

  constructor(opts: StreamCaptureOptions) {
    this.opts = opts;
    this.codec = opts.codec === 0 ? VideoCodec.H264_HW : opts.codec;
  }

  /// Prompt the user via Chromium's native screen-share dialog, set up
  /// the encoder, and start pumping encoded chunks to native. Returns
  /// the actual capture dimensions Chromium negotiated, so the caller
  /// can announce them to the server with truthful values (the
  /// pre-capture dims passed via opts are best-guess).
  async start(): Promise<{ width: number; height: number }> {
    // Native encode path: skip the renderer-encoded path entirely.
    // Native owns capture + color convert + encode (FFmpeg NVENC/AMF on
    // Windows; NVENC/VAAPI on Linux) + UDP + self-preview fan-out. We
    // kick off `start_screen_share` and let native run until `stop()`.
    // Frames flow back for self-preview through the same per-stream
    // Buffer TSFN as remote streams (StreamVideoPlayer subscribes by
    // username). On Windows the source id comes from the in-app picker
    // (WGC opens it); on Linux it's omitted — the XDG portal's own
    // dialog selects the source.
    if (isNativeEncodeActive()) {
      this.usedNative = true;
      const isWindows = window.decibell.platform === "win32";
      if (isWindows && !this.opts.sourceId) {
        throw new Error("sourceId required on Windows");
      }
      try {
        await invoke("start_screen_share", {
          serverId: this.opts.serverId,
          channelId: this.opts.channelId,
          sourceId: this.opts.sourceId,
          fps: this.opts.fps,
          width: this.opts.width,
          height: this.opts.height,
          videoBitrateKbps: this.opts.bitrateKbps,
          shareAudio: this.opts.shareAudio,
          audioBitrateKbps: this.opts.audioBitrateKbps,
          audioMode: this.opts.audioMode,
          audioApps: this.opts.audioApps,
          initialCodec: this.codec,
          enforcedCodec: this.codec,
          // True = native owns capture + encode (Windows WGC/FFmpeg,
          // Linux PipeWire/FFmpeg). The renderer WebCodecs fallback
          // below re-invokes via CaptureSourcePicker with false.
          nativeEncode: true,
          includeCursor: this.opts.includeCursor,
        });
        console.log(
          `[StreamCapture/native] pipeline started at ${this.opts.width}x${this.opts.height}@${this.opts.fps} (codec=${this.codec}, platform=${window.decibell.platform})`,
        );
        return { width: this.opts.width, height: this.opts.height };
      } catch (e) {
        // Native pipeline failed to start (encoder open, capture init,
        // no HW encoder on this GPU/driver…). Fall back to the renderer
        // WebCodecs path instead of failing Go Live outright — OpenH264
        // software encode exists everywhere in this castlabs build.
        // markNativeEncodeFailed flips isNativeEncodeActive() for the
        // session so CaptureSourcePicker's signalling branch and the
        // player's self-preview path agree on the active pipeline.
        console.error(
          "[StreamCapture] native pipeline failed; falling back to WebCodecs:",
          e,
        );
        markNativeEncodeFailed();
        this.usedNative = false;
        if (this.codec === VideoCodec.H265) {
          // WebCodecs HEVC encode needs platform support that is
          // usually absent; H.264 always has the OpenH264 floor.
          this.codec = VideoCodec.H264_HW;
        }
        toast.warning(
          "GPU streaming pipeline unavailable",
          "Falling back to software encoding. Stream audio may be unavailable.",
        );
      }
    }

    // Pre-stash the chosen source id so the main-process
    // setDisplayMediaRequestHandler picks it out of desktopCapturer
    // instead of auto-picking sources[0]. Chrome 108+ restricted the
    // legacy `chromeMediaSource: 'desktop'` getUserMedia path; the
    // supported route is getDisplayMedia + handler. The pre-stash is
    // cleared either by a successful handler pick or by our finally
    // below if getDisplayMedia rejects (cancel / NotAllowedError).
    if (this.opts.sourceId) {
      await window.decibell.capture.setNextSource(this.opts.sourceId);
    }
    const videoConstraints: MediaTrackConstraints = {
      frameRate: this.opts.fps,
    };
    if (!this.opts.useNativeSize) {
      videoConstraints.width = this.opts.width;
      videoConstraints.height = this.opts.height;
    }
    try {
      this.stream = await navigator.mediaDevices.getDisplayMedia({
        video: videoConstraints,
        audio: this.opts.shareAudio,
      });
    } catch (e) {
      if (this.opts.sourceId) {
        await window.decibell.capture.setNextSource(null).catch(() => {});
      }
      throw e;
    }

    const track = this.stream.getVideoTracks()[0];
    if (!track) {
      throw new Error("No video track in display media");
    }
    const settings = track.getSettings();
    console.log("[StreamCapture] track ready:", {
      width: settings.width,
      height: settings.height,
      frameRate: settings.frameRate,
      muted: track.muted,
      readyState: track.readyState,
      label: track.label,
      useNativeSize: this.opts.useNativeSize ?? false,
    });
    track.addEventListener("ended", () => {
      console.log("[StreamCapture] track ended event fired");
      this.opts.onCaptureEnded?.();
      this.stop().catch(() => {});
    });
    track.addEventListener("mute", () => {
      console.warn("[StreamCapture] track muted");
    });
    track.addEventListener("unmute", () => {
      console.log("[StreamCapture] track unmuted");
    });

    // Set up the frame reader BEFORE configuring the encoder so we can
    // peek the first VideoFrame and use its actual dimensions. We
    // can't trust track.getSettings() on Wayland: Chromium's PipeWire
    // integration reports the compositor's canvas size (e.g., 2560×1440
    // when the primary monitor is 1440p) even when the picked source
    // is a 1080p surface that produces 1920×1080 frames. Configuring
    // the encoder off getSettings then producing a stream with 1080p
    // content padded to 1440p with black borders is exactly what
    // happens. The first VideoFrame's codedWidth/codedHeight is the
    // source of truth.
    type ProcessorCtor = new (init: { track: MediaStreamTrack }) => {
      readable: ReadableStream<VideoFrame>;
    };
    const Processor = (window as unknown as { MediaStreamTrackProcessor?: ProcessorCtor })
      .MediaStreamTrackProcessor;
    if (!Processor) {
      throw new Error("MediaStreamTrackProcessor not available");
    }
    const processor = new Processor({ track });
    this.reader = processor.readable.getReader();

    // The first read gets a hard timeout: a fully-occluded or minimized
    // window on some compositors never produces a frame, and awaiting it
    // forever left Go Live stuck with no error and the capture running.
    let firstFrameTimer: number | undefined;
    const firstFrameTimeout = new Promise<never>((_, reject) => {
      firstFrameTimer = window.setTimeout(
        () =>
          reject(
            new Error(
              "Capture produced no frames within 5s — the selected window may be minimized or protected",
            ),
          ),
        5000,
      );
    });
    let firstRead: ReadableStreamReadResult<VideoFrame>;
    try {
      firstRead = await Promise.race([this.reader.read(), firstFrameTimeout]);
    } catch (e) {
      void this.stop();
      throw e;
    } finally {
      window.clearTimeout(firstFrameTimer);
    }
    if (firstRead.done || !firstRead.value) {
      throw new Error("Capture track produced no frames");
    }
    const firstFrame = firstRead.value;
    const captureWidth = firstFrame.codedWidth;
    const captureHeight = firstFrame.codedHeight;
    if (
      settings.width !== captureWidth ||
      settings.height !== captureHeight
    ) {
      console.log(
        `[StreamCapture] track.getSettings() reported ` +
          `${settings.width}x${settings.height}, but first frame is ` +
          `${captureWidth}x${captureHeight} — using first-frame dims for encoder.`,
      );
    }

    const codecString = webCodecsStringForCodec(
      this.codec,
      captureWidth,
      captureHeight,
      this.opts.fps,
    );
    const encoderConfig: VideoEncoderConfig = {
      codec: codecString,
      width: captureWidth,
      height: captureHeight,
      framerate: this.opts.fps,
      bitrate: this.opts.bitrateKbps * 1000,
      latencyMode: "realtime",
      hardwareAcceleration:
        this.codec === VideoCodec.H264_SW ? "prefer-software" : "prefer-hardware",
    };

    // Soft pre-flight: confirm the codec family + level + bitrate are
    // even accepted at this resolution. Strips `latencyMode` and
    // `hardwareAcceleration` because Chromium treats those as hard
    // constraints in `isConfigSupported`; we want this check to mirror
    // the *configure()* semantic (hint, not constraint).
    const preflight = await VideoEncoder.isConfigSupported({
      codec: encoderConfig.codec,
      width: encoderConfig.width,
      height: encoderConfig.height,
      framerate: encoderConfig.framerate,
      bitrate: encoderConfig.bitrate,
    });
    if (!preflight.supported) {
      firstFrame.close();
      throw new Error(
        `Encoder config not supported: codec=${codecString} ${captureWidth}x${captureHeight}@${this.opts.fps} ${this.opts.bitrateKbps}kbps`,
      );
    }

    // Strict HW pre-flight along two axes:
    //
    //   1. Codec-string ladder. NVENC / Intel QSV / AMD AMF report
    //      only a subset of the silicon's actual H.264/HEVC levels
    //      through their Media Foundation wrappers; walking up to a
    //      higher level the MFT does advertise unblocks the path
    //      without changing the encoded bitstream.
    //
    //   2. latencyMode. WebCodecs spec says it's a hint, but
    //      Chromium's MFT NVENC binding on Windows enforces it as
    //      a hard constraint at 60fps and refuses every codec/level
    //      combination when latencyMode=realtime is set. Dropping
    //      the hint lets NVENC pick its default rate-control mode
    //      and accept the config. HW H.264 with a slightly larger
    //      encoder buffer beats SW H.264 with realtime mode for
    //      screen-share CPU usage — gaming streamers care about the
    //      CPU floor more than the last 10ms of buffer latency.
    //
    // HW is only accepted when both `supported: true` AND the
    // negotiated `hardwareAcceleration` returned by isConfigSupported
    // is 'prefer-hardware' — Chromium silently downgrades the hint
    // while still answering supported: true, so the negotiated field
    // is the truthful signal.
    if (encoderConfig.hardwareAcceleration === "prefer-hardware") {
      const codecLadder = codecLevelLadder(codecString);
      // Preferred order: realtime → default. The default ("quality")
      // mode adds a few frames of encoder buffer — fine for screen
      // share. Only used as a fallback when realtime + HW refused.
      const latencyOptions: (VideoEncoderConfig["latencyMode"] | undefined)[] =
        encoderConfig.latencyMode === "realtime"
          ? ["realtime", undefined]
          : [encoderConfig.latencyMode];
      let chosen: {
        codec: string;
        latencyMode: VideoEncoderConfig["latencyMode"] | undefined;
      } | null = null;
      outer: for (const latency of latencyOptions) {
        for (const candidate of codecLadder) {
          const probeConfig: VideoEncoderConfig = {
            codec: candidate,
            width: encoderConfig.width,
            height: encoderConfig.height,
            framerate: encoderConfig.framerate,
            bitrate: encoderConfig.bitrate,
            hardwareAcceleration: "prefer-hardware",
          };
          if (latency) probeConfig.latencyMode = latency;
          const probe = await VideoEncoder.isConfigSupported(probeConfig);
          const negotiated = probe.config?.hardwareAcceleration;
          const ok = probe.supported && negotiated === "prefer-hardware";
          console.log(
            `[StreamCapture] HW pre-flight: codec=${candidate} ` +
              `${captureWidth}x${captureHeight}@${this.opts.fps} ` +
              `${this.opts.bitrateKbps}kbps latency=${latency ?? "default"} → ` +
              `supported=${probe.supported}, negotiated=${negotiated}` +
              (ok ? " ✓ HW viable" : ""),
          );
          if (ok) {
            chosen = { codec: candidate, latencyMode: latency };
            break outer;
          }
        }
      }
      if (chosen) {
        if (chosen.codec !== codecString) {
          console.log(
            `[StreamCapture] codec swap: ${codecString} → ${chosen.codec} ` +
              `for MFT compatibility.`,
          );
        }
        if (chosen.latencyMode !== encoderConfig.latencyMode) {
          console.log(
            `[StreamCapture] latencyMode swap: ${encoderConfig.latencyMode} → ` +
              `${chosen.latencyMode ?? "default"} so NVENC will accept the config.`,
          );
        }
        encoderConfig.codec = chosen.codec;
        if (chosen.latencyMode) {
          encoderConfig.latencyMode = chosen.latencyMode;
        } else {
          delete encoderConfig.latencyMode;
        }
      } else {
        console.warn(
          `[StreamCapture] no HW path for ${codecString} at ` +
            `${captureWidth}x${captureHeight}@${this.opts.fps} ${this.opts.bitrateKbps}kbps ` +
            `across the codec ladder × latencyMode axes; using prefer-software.`,
        );
        encoderConfig.hardwareAcceleration = "prefer-software";
      }
    }

    const buildEncoder = (): VideoEncoder =>
      new VideoEncoder({
        output: (chunk, metadata) => {
          if (this.stopping) return;
          if (metadata?.decoderConfig?.description) {
            const desc = metadata.decoderConfig.description as
              | ArrayBuffer
              | ArrayBufferView;
            // For an ArrayBufferView (Chromium hands AV1 / HEVC
            // descriptions back this way) we MUST honour byteOffset +
            // byteLength — `desc.buffer.slice(0)` would clone the
            // entire underlying buffer, including bytes outside the
            // view's window, and the decoder would receive garbage.
            this.description =
              desc instanceof ArrayBuffer
                ? new Uint8Array(desc.slice(0))
                : new Uint8Array(
                    desc.buffer.slice(
                      desc.byteOffset,
                      desc.byteOffset + desc.byteLength,
                    ),
                  );
            if (!this.descriptionLogged) {
              console.log(
                `[StreamCapture] decoder description captured ` +
                  `(codec=${this.codec}, size=${this.description.byteLength})`,
              );
              this.descriptionLogged = true;
            }
          }

          const data = new Uint8Array(chunk.byteLength);
          chunk.copyTo(data);
          const isKey = chunk.type === "key";

          // Self-preview fan-out: ship a copy of the encoded chunk to
          // any local subscribers (StreamVideoPlayer when the user is
          // watching their own stream). Skips the wire so frames are
          // visible on a single machine even with no other watchers.
          if (localFrameSubs.size > 0) {
            const localFrame: LocalEncodedFrame = {
              codec: this.codec,
              keyframe: isKey,
              timestamp: chunk.timestamp,
              data,
              description: isKey && this.description ? this.description : null,
            };
            for (const sub of localFrameSubs) sub(localFrame);
          }

          // Fire-and-forget; awaiting per-frame would back-pressure the
          // encoder output queue. Native invoke is non-blocking.
          //
          // Important: omit `description` entirely when we don't have
          // one (non-keyframes, or H.264 keyframes with inline SPS/PPS
          // in Annex B). napi-rs's `Option<Buffer>` deserializer tries
          // to create a Buffer reference from `null` and throws
          // "Failed to create reference from Buffer"; it handles
          // `undefined` (i.e., a missing field) correctly.
          const args: {
            codec: VideoCodec;
            keyframe: boolean;
            data: Uint8Array;
            description?: Uint8Array;
          } = { codec: this.codec, keyframe: isKey, data };
          if (isKey && this.description) {
            args.description = this.description;
          }
          invoke("send_video_frame", args).catch((e) =>
            console.error("[StreamCapture] send_video_frame failed:", e),
          );
        },
        error: (e) => this.handleEncoderError(e, codecString),
      });

    // Hardware-acceleration fallback: if the user picked H264_HW or
    // HEVC and Chromium can't actually allocate a hardware encoder
    // (Linux+NVIDIA without nvidia-vaapi-driver is the common case),
    // the first `configure()` succeeds but encoder construction fails
    // asynchronously with OperationError. Try prefer-hardware first,
    // then transparently retry with prefer-software on async failure.
    // For codec=H264_SW we go straight to prefer-software.
    this.encoder = buildEncoder();
    this.preferHardwareTried = encoderConfig.hardwareAcceleration === "prefer-hardware";
    this.encoderConfig = encoderConfig;
    this.buildEncoder = buildEncoder;
    this.encoder.configure(encoderConfig);

    // Encode the peeked first frame as a keyframe, then hand off to
    // the pump for the rest. The reader is already attached above.
    try {
      this.encoder.encode(firstFrame, { keyFrame: true });
      this.wantKeyframe = false;
    } finally {
      firstFrame.close();
    }

    void this.pumpLoop();

    return { width: captureWidth, height: captureHeight };
  }

  private async pumpLoop(): Promise<void> {
    if (!this.reader || !this.encoder) return;
    let firstFrameLogged = false;
    let framesSinceLastReport = 0;
    let lastReportAt = Date.now();
    // Rate gate: the track's `frameRate` constraint is only a request —
    // Chromium can still hand us frames above the target (a 144 Hz
    // monitor on a 60 fps stream), and every one of those is an encode.
    // Deadline pacing with half-an-interval tolerance, so a source that
    // runs at exactly the target isn't halved by jitter (accept/drop
    // alternation) and a faster source is thinned to the target rate.
    const intervalUs = 1_000_000 / Math.max(1, this.opts.fps);
    let nextDueUs = 0;
    try {
      while (!this.stopping) {
        const { value: frame, done } = await this.reader.read();
        if (done) {
          console.log("[StreamCapture] reader signalled done — track ended");
          break;
        }
        if (!frame) continue;
        if (!firstFrameLogged) {
          console.log(
            `[StreamCapture] first frame from track ` +
              `(${frame.codedWidth}x${frame.codedHeight})`,
          );
          firstFrameLogged = true;
        }
        const tsUs = frame.timestamp ?? performance.now() * 1000;
        if (tsUs < nextDueUs - intervalUs / 2) {
          frame.close();
          continue;
        }
        nextDueUs =
          tsUs > nextDueUs + intervalUs * 2 ? tsUs + intervalUs : nextDueUs + intervalUs;
        framesSinceLastReport += 1;
        try {
          // Encoder backpressure protection: drop frames if the queue
          // is too deep. The queue check has to gate `wantKeyframe`
          // consumption too — otherwise a requested keyframe can be
          // silently swallowed during a spike and watchers stay stuck
          // on the previous GOP until the next natural IDR.
          if (this.encoder.encodeQueueSize < 4) {
            const encodeOpts: VideoEncoderEncodeOptions = {};
            if (this.wantKeyframe) {
              encodeOpts.keyFrame = true;
              this.wantKeyframe = false;
            }
            this.encoder.encode(frame, encodeOpts);
          }
          // Periodic thumbnail. drawImage on a VideoFrame is sync,
          // so the bitmap is baked into the canvas before frame.close()
          // in the finally below races ahead. The async convertToBlob
          // works on the canvas alone — it doesn't need the frame.
          this.maybeCaptureThumbnail(frame);
        } finally {
          frame.close();
        }
        const now = Date.now();
        if (now - lastReportAt > 5000) {
          console.log(
            `[StreamCapture] last 5s: ${framesSinceLastReport} frames captured ` +
              `(queueSize=${this.encoder.encodeQueueSize})`,
          );
          framesSinceLastReport = 0;
          lastReportAt = now;
        }
      }
    } catch (e) {
      if (!this.stopping) {
        console.error("[StreamCapture] pump loop error:", e);
      }
    }
  }

  /// Throttled JPEG thumbnail capture. Call ONCE PER pump-loop iteration
  /// — the rate limit is enforced internally so callers don't have to
  /// time anything. Synchronous draw to OffscreenCanvas (so the caller
  /// can frame.close() right after) followed by an async JPEG encode
  /// + IPC send. The in-flight guard prevents pile-up if convertToBlob
  /// or the IPC ever stalls.
  private maybeCaptureThumbnail(frame: VideoFrame): void {
    // Thumbnails only exist for community voice channels.
    if (!this.opts.serverId || !this.opts.channelId) return;
    const now = performance.now();
    if (now - this.lastThumbnailAt < StreamCapture.THUMBNAIL_INTERVAL_MS) return;
    if (this.thumbnailInFlight) return;
    if (!frame.codedWidth || !frame.codedHeight) return;
    this.lastThumbnailAt = now;

    // Compute target dims: longest edge clamped to THUMBNAIL_MAX_EDGE.
    // OffscreenCanvas is reused across calls; only re-allocated when
    // the source aspect ratio changes (resolution adjustments mid-
    // stream from the LCD codec picker, etc.).
    const srcW = frame.codedWidth;
    const srcH = frame.codedHeight;
    let targetW: number, targetH: number;
    if (srcW >= srcH) {
      targetW = Math.min(srcW, StreamCapture.THUMBNAIL_MAX_EDGE);
      targetH = Math.max(1, Math.round((targetW * srcH) / srcW));
    } else {
      targetH = Math.min(srcH, StreamCapture.THUMBNAIL_MAX_EDGE);
      targetW = Math.max(1, Math.round((targetH * srcW) / srcH));
    }
    if (
      !this.thumbnailCanvas ||
      this.thumbnailCanvas.width !== targetW ||
      this.thumbnailCanvas.height !== targetH
    ) {
      this.thumbnailCanvas = new OffscreenCanvas(targetW, targetH);
    }
    const ctx = this.thumbnailCanvas.getContext("2d");
    if (!ctx) return;
    try {
      ctx.drawImage(frame, 0, 0, targetW, targetH);
    } catch (e) {
      console.warn("[StreamCapture] thumbnail draw failed:", e);
      return;
    }

    this.thumbnailInFlight = true;
    void this.thumbnailCanvas
      .convertToBlob({ type: "image/jpeg", quality: 0.7 })
      .then(async (blob) => {
        const buf = await blob.arrayBuffer();
        await invoke("send_stream_thumbnail", {
          serverId: this.opts.serverId,
          channelId: this.opts.channelId,
          jpegData: new Uint8Array(buf),
        }).catch(() => {});
      })
      .catch((e) => {
        console.warn("[StreamCapture] thumbnail encode failed:", e);
      })
      .finally(() => {
        this.thumbnailInFlight = false;
      });
  }

  private handleEncoderError(e: unknown, codecString: string): void {
    if (this.stopping) return;

    // If we tried prefer-hardware and that failed asynchronously
    // (Chromium accepted the config but couldn't actually allocate a
    // hardware encoder — typical on Linux+NVIDIA without
    // nvidia-vaapi-driver), transparently retry with prefer-software.
    // OpenH264 / libaom always succeed where the codec exists at all.
    if (
      this.preferHardwareTried &&
      this.encoderConfig &&
      this.buildEncoder &&
      !this.stopping
    ) {
      console.warn(
        `[StreamCapture] hardware encoder failed (${codecString}); retrying with prefer-software:`,
        e,
      );
      this.preferHardwareTried = false;
      const retryConfig: VideoEncoderConfig = {
        ...this.encoderConfig,
        hardwareAcceleration: "prefer-software",
      };
      this.encoderConfig = retryConfig;
      try {
        if (this.encoder && this.encoder.state !== "closed") {
          this.encoder.close();
        }
      } catch {
        // ignore — closing an already-errored encoder may throw
      }
      try {
        this.encoder = this.buildEncoder();
        this.encoder.configure(retryConfig);
        this.wantKeyframe = true;
        const human = videoCodecHumanName(this.codec);
        toast.warning(
          `GPU ${human} encoder unavailable`,
          `Streaming with software ${human}. Expect higher CPU usage.`,
        );
        return;
      } catch (retryErr) {
        console.error(
          `[StreamCapture] prefer-software retry also failed (${codecString}):`,
          retryErr,
        );
      }
    }

    console.error(
      `[StreamCapture] encoder error (codec=${codecString} ${this.opts.width}x${this.opts.height}@${this.opts.fps}):`,
      e,
    );
    toast.error(
      "Stream stopped",
      `${videoCodecHumanName(this.codec)} encoder failed and could not be recovered.`,
    );
    this.stopping = true;
    this.opts.onCaptureEnded?.();
    this.stop().catch(() => {});
  }

  /// Force the next encoded frame to be a keyframe. Called from the
  /// `keyframe_requested` event when a watcher (or the codec selector)
  /// asks for one.
  forceKeyframe(): void {
    this.wantKeyframe = true;
  }

  async stop(): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    // Native pipeline (Windows always; Linux when a HW encoder was used):
    // nothing renderer-side to tear down (no reader/encoder/stream). Drive
    // stop_screen_share here so the native engine is torn down even on
    // error paths that call stopActiveStream() without a separate
    // stop_screen_share (e.g. a start failure). It's idempotent, so the
    // external stop callers (UserPanel / VoicePanel / leave handler /
    // onCaptureEnded) invoking it too is harmless.
    if (this.usedNative) {
      // Awaited so a caller sequencing stop→start (settings change →
      // restart) can't have this stop land after — and tear down — the
      // next session's start_screen_share.
      await invoke("stop_screen_share", {
        serverId: this.opts.serverId,
        channelId: this.opts.channelId,
      }).catch(() => {});
      return;
    }
    try {
      this.reader?.cancel().catch(() => {});
    } catch {
      // ignore
    }
    this.reader = null;
    if (this.encoder && this.encoder.state !== "closed") {
      try {
        this.encoder.close();
      } catch {
        // ignore
      }
    }
    this.encoder = null;
    if (this.stream) {
      for (const track of this.stream.getTracks()) {
        track.stop();
      }
      this.stream = null;
    }
  }

  /// Move an already-running stream to a different voice channel WITHOUT
  /// restarting capture (so no getDisplayMedia / portal / WGC re-prompt).
  /// Native re-points the video (and stream-audio) send sockets at the new
  /// voice channel's UDP sockets and re-announces the stream — capture and
  /// the encoder keep running untouched on BOTH the WebCodecs and native
  /// paths. Watchers in the new channel catch up on the forced keyframe.
  async retarget(serverId: string, channelId: string): Promise<void> {
    if (this.stopping) return;

    // Use the negotiated encoder dims when we have them (WebCodecs path);
    // fall back to the requested dims for the native path (encoderConfig is
    // null there — native owns the real dimensions).
    const width = this.encoderConfig?.width ?? this.opts.width;
    const height = this.encoderConfig?.height ?? this.opts.height;

    await invoke("move_stream_to_channel", {
      serverId,
      channelId,
      fps: this.opts.fps,
      width,
      height,
      videoBitrateKbps: this.opts.bitrateKbps,
      shareAudio: this.opts.shareAudio,
      initialCodec: this.codec,
      // Keep the codec we're already encoding with — don't renegotiate mid-move.
      enforcedCodec: this.codec,
    });

    // Point future frames / thumbnails / stop at the new channel.
    this.opts.serverId = serverId;
    this.opts.channelId = channelId;

    // WebCodecs path: force a keyframe from the renderer encoder so the new
    // channel's watchers get an IDR immediately. (The native path forces its
    // own keyframe inside move_stream_to_channel.)
    this.forceKeyframe();
  }
}

// Module-level singleton — there's only ever one active screen-share
// session in the app. UserPanel's "Stop sharing" button calls
// `stopActiveStream()` to tear it down without needing to pass the
// instance through the component tree.
let active: StreamCapture | null = null;

export async function startActiveStream(
  opts: StreamCaptureOptions,
): Promise<StreamCapture> {
  if (active) {
    // Await the old session's teardown. Fire-and-forget here let the old
    // stop_screen_share race the new start_screen_share and, when it
    // landed second, tear down the session that had just started.
    await active.stop().catch(() => {});
  }
  active = new StreamCapture(opts);
  return active;
}

export async function stopActiveStream(): Promise<void> {
  const cur = active;
  active = null;
  if (cur) await cur.stop();
}

export function activeStreamCapture(): StreamCapture | null {
  return active;
}

/// desktopCapturer id of the source the live stream is capturing (Windows
/// custom picker only; undefined elsewhere). The live stream-audio popover
/// passes it to `list_stream_audio_apps` so the window's app gets flagged.
export function activeStreamSourceId(): string | undefined {
  return active?.sourceId;
}

/// Codec-string fallback ladder for the HW pre-flight. Given the
/// spec-correct codec string we want to use, returns it plus any
/// higher-level alternatives that should still produce the same
/// bitstream from the codec's silicon. The MFT wrappers in Chromium
/// on Windows (NVENC/QSV/AMF for H.264 and HEVC) sometimes enumerate
/// only a subset of the silicon's actual level support — picking a
/// higher level the MFT does advertise unlocks the HW path without
/// changing what the wire bitstream looks like.
///
/// AV1 already uses a single Level (av01.0.08M.08 ≈ Level 4.0) that
/// covers everything we offer up through 4K60 — no ladder needed.
function codecLevelLadder(base: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  const add = (s: string): void => {
    if (!seen.has(s)) {
      seen.add(s);
      out.push(s);
    }
  };
  add(base);
  // H.264 High Profile — strings of the form `avc1.6400XX` where XX
  // is the level_idc in hex. Ladder: spec-correct → 5.0 → 5.1 → 5.2.
  // Stays inside Level 5.x so we don't claim Level 6.0+ which much
  // pre-Ampere NVENC silicon doesn't advertise.
  if (base.startsWith("avc1.6400")) {
    add("avc1.640032"); // 5.0
    add("avc1.640033"); // 5.1
    add("avc1.640034"); // 5.2
  }
  // HEVC Main Profile — strings of the form `hvc1.1.6.LXXX.B0` where
  // XXX is level*30. Ladder: spec-correct → L120 (4.0) → L150 (5.0) →
  // L153 (5.1) → L156 (5.2).
  if (base.startsWith("hvc1.1.6.L")) {
    add("hvc1.1.6.L120.B0"); // 4.0
    add("hvc1.1.6.L150.B0"); // 5.0
    add("hvc1.1.6.L153.B0"); // 5.1
    add("hvc1.1.6.L156.B0"); // 5.2
  }
  return out;
}

function webCodecsStringForCodec(
  codec: VideoCodec,
  width: number,
  height: number,
  fps: number = 30,
): string {
  switch (codec) {
    case VideoCodec.AV1: {
      // Suppress unused-arg warnings for the resolution-aware branch
      // we used to take here. AV1 codec-string format is
      // `av01.<profile>.<seq_level_idx><tier>.<bit_depth>`. The level
      // field is the *index* (0-31), not the human level number — so
      // idx 4 is Level 3.0 (1.5 M pixel cap, doesn't even fit 1080p),
      // not Level 4.0. The previous per-resolution table picked
      // indices 4-10 thinking they meant L4.0-L6.0, which produced
      // codec strings claiming a level the bitstream didn't fit; the
      // encoder either failed outright or produced a non-conformant
      // stream the decoder rejected.
      //
      // Level 4.0 (idx 8, Main tier, 8-bit) covers everything we
      // offer up through 4K60: max display rate 1.23 G samples/s,
      // max picture size 8.9 M pixels, max h_size 4096, max v_size
      // 2304 (per AV1 spec Annex A.3). Always use it.
      void width;
      void height;
      void fps;
      return "av01.0.08M.08";
    }
    case VideoCodec.H265: {
      // HEVC Main profile, level from the spec's Table A.8 limits:
      // pick the smallest level whose MaxLumaPs (picture size) AND
      // MaxLumaSr (samples/second) both hold. The old fps>30 split
      // under-declared throughput-heavy combos — 720p60 already
      // exceeded L3.1's MaxLumaSr, and every 120fps mode was wrong.
      // Level string is `L{level*30}`.
      const lumaPs = width * height;
      const lumaSr = lumaPs * fps;
      const hevcLevels: { name: string; maxPs: number; maxSr: number }[] = [
        { name: "L93.B0", maxPs: 983_040, maxSr: 33_177_600 }, // 3.1
        { name: "L120.B0", maxPs: 2_228_224, maxSr: 66_846_720 }, // 4.0
        { name: "L123.B0", maxPs: 2_228_224, maxSr: 133_693_440 }, // 4.1
        { name: "L150.B0", maxPs: 8_912_896, maxSr: 534_773_760 }, // 5.0
        { name: "L153.B0", maxPs: 8_912_896, maxSr: 1_069_547_520 }, // 5.1
        { name: "L156.B0", maxPs: 8_912_896, maxSr: 2_139_095_040 }, // 5.2
      ];
      const hevcPick =
        hevcLevels.find((l) => lumaPs <= l.maxPs && lumaSr <= l.maxSr) ??
        hevcLevels[hevcLevels.length - 1];
      return `hvc1.1.6.${hevcPick.name}`;
    }
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW:
    default: {
      // H.264 High Profile (`6400`) + level_idc in hex, from the
      // spec's Table A-1: smallest level whose MaxFS (frame size in
      // macroblocks) AND MaxMBPS (macroblocks/second) both hold.
      // Replaces the fps>30 split, which picked levels valid at 60fps
      // but under the throughput limit at 120fps (720p120 declared
      // L3.2 needing L4.2; 1080p120 declared L4.2 needing L5.1;
      // 1440p120 declared L5.1 needing L5.2) — and per §5.3 of the
      // handoff, an under-declared level fails isConfigSupported at
      // the real resolution, killing Go Live.
      const mbs = Math.ceil(width / 16) * Math.ceil(height / 16);
      const mbps = mbs * fps;
      const avcLevels: { idc: string; maxFs: number; maxMbps: number }[] = [
        { idc: "1F", maxFs: 3_600, maxMbps: 108_000 }, // 3.1
        { idc: "20", maxFs: 5_120, maxMbps: 216_000 }, // 3.2
        { idc: "28", maxFs: 8_192, maxMbps: 245_760 }, // 4.0
        { idc: "2A", maxFs: 8_704, maxMbps: 522_240 }, // 4.2
        { idc: "32", maxFs: 22_080, maxMbps: 589_824 }, // 5.0
        { idc: "33", maxFs: 36_864, maxMbps: 983_040 }, // 5.1
        { idc: "34", maxFs: 36_864, maxMbps: 2_073_600 }, // 5.2
      ];
      const avcPick =
        avcLevels.find((l) => mbs <= l.maxFs && mbps <= l.maxMbps) ??
        avcLevels[avcLevels.length - 1]; // 4K120 exceeds 5.2 — clamp, caps never offer it
      return `avc1.6400${avcPick.idc}`;
    }
  }
}
