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
import { VideoCodec } from "../../../types";
import { toast } from "../../../stores/toastStore";
import { videoCodecHumanName } from "../../../utils/codecMap";

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
  /// Routing for the periodic JPEG thumbnail the streamer broadcasts
  /// to non-watching voice-channel participants (so they see a poster
  /// image on the participant tile instead of a black square). The
  /// pump loop draws every Nth frame to an OffscreenCanvas, encodes
  /// it as JPEG, and ships it via `send_stream_thumbnail`.
  serverId: string;
  channelId: string;
  /// When true, getDisplayMedia is requested without width/height
  /// constraints so Chromium delivers the captured surface at its
  /// native resolution (e.g. 2560×1440 on a 1440p monitor). The
  /// encoder is then configured with the negotiated dimensions read
  /// off the track. When false, width/height are passed through as
  /// hard constraints and Chromium scales the surface to match.
  useNativeSize?: boolean;
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
  private frameCounter = 0;
  private stopping = false;
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
    const videoConstraints: MediaTrackConstraints = {
      frameRate: this.opts.fps,
    };
    if (!this.opts.useNativeSize) {
      videoConstraints.width = this.opts.width;
      videoConstraints.height = this.opts.height;
    }
    this.stream = await navigator.mediaDevices.getDisplayMedia({
      video: videoConstraints,
      audio: this.opts.shareAudio,
    });

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

    const firstRead = await this.reader.read();
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

    // Pre-flight: even with castlabs Chromium, a config that survived
    // the boot-time encoderProbe (run at 1280×720@30) can still be
    // rejected at the actual stream resolution / bitrate / codec
    // level. Failing fast here yields a meaningful error instead of an
    // async OperationError that strands the watcher on a spinner.
    //
    // Critically: we strip `latencyMode` and `hardwareAcceleration`
    // from the pre-flight config. Chromium treats those as hard
    // constraints in `isConfigSupported` (e.g., `prefer-hardware` for
    // H.264 on Linux+NVIDIA returns `supported: false` because there's
    // no VAAPI driver) but as soft *hints* during the actual
    // `configure()` call, where it falls back to software. We want the
    // pre-flight to mirror that softer semantic — confirm the codec
    // family + level + bitrate are accepted, then let configure()
    // decide hardware vs software.
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
    this.frameCounter += 1;

    void this.pumpLoop();

    return { width: captureWidth, height: captureHeight };
  }

  private async pumpLoop(): Promise<void> {
    if (!this.reader || !this.encoder) return;
    let firstFrameLogged = false;
    let framesSinceLastReport = 0;
    let lastReportAt = Date.now();
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
        this.frameCounter += 1;
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
}

// Module-level singleton — there's only ever one active screen-share
// session in the app. UserPanel's "Stop sharing" button calls
// `stopActiveStream()` to tear it down without needing to pass the
// instance through the component tree.
let active: StreamCapture | null = null;

export function startActiveStream(opts: StreamCaptureOptions): StreamCapture {
  if (active) {
    void active.stop();
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
    case VideoCodec.H265:
      // HEVC Main profile. Level encoded as `L{level*30}`: L93=3.1,
      // L120=4.0, L150=5.0, L153=5.1, L156=5.2.
      if (width * height >= 3840 * 2160 && fps > 30) return "hvc1.1.6.L156.B0";
      if (width * height >= 3840 * 2160) return "hvc1.1.6.L153.B0";
      if (width * height >= 2560 * 1440) return "hvc1.1.6.L150.B0";
      if (width * height >= 1920 * 1080 && fps > 30) return "hvc1.1.6.L153.B0";
      if (width * height >= 1920 * 1080) return "hvc1.1.6.L123.B0"; // 4.1
      return "hvc1.1.6.L93.B0"; // 3.1 — 720p
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW:
    default: {
      // H.264 High Profile (`6400`) + level. The level digit is hex
      // so 0x28=40=Level 4.0, 0x2A=42=Level 4.2, 0x32=50=Level 5.0,
      // 0x34=52=Level 5.2.
      //   - Level 3.1: 720p30
      //   - Level 3.2: 720p60
      //   - Level 4.0: 1080p30
      //   - Level 4.2: 1080p60
      //   - Level 5.0: 1440p30
      //   - Level 5.1: 4K30
      //   - Level 5.2: 4K60
      if (width * height >= 3840 * 2160 && fps > 30) return "avc1.640034"; // 5.2
      if (width * height >= 3840 * 2160) return "avc1.640033"; // 5.1
      if (width * height >= 2560 * 1440 && fps > 30) return "avc1.640033"; // 5.1
      if (width * height >= 2560 * 1440) return "avc1.640032"; // 5.0
      if (width * height >= 1920 * 1080 && fps > 30) return "avc1.64002A"; // 4.2
      if (width * height >= 1920 * 1080) return "avc1.640028"; // 4.0
      if (width * height >= 1280 * 720 && fps > 30) return "avc1.640020"; // 3.2 (was 0x21 — invalid level_idc)
      return "avc1.64001F"; // 3.1 — 720p
    }
  }
}
