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
  private codec: VideoCodec;
  private opts: StreamCaptureOptions;
  private wantKeyframe = true;
  private frameCounter = 0;
  private stopping = false;
  private encoderConfig: VideoEncoderConfig | null = null;
  private buildEncoder: (() => VideoEncoder) | null = null;
  private preferHardwareTried = false;

  constructor(opts: StreamCaptureOptions) {
    this.opts = opts;
    this.codec = opts.codec === 0 ? VideoCodec.H264_HW : opts.codec;
  }

  /// Prompt the user via Chromium's native screen-share dialog, set up
  /// the encoder, and start pumping encoded chunks to native.
  async start(): Promise<void> {
    this.stream = await navigator.mediaDevices.getDisplayMedia({
      video: {
        width: this.opts.width,
        height: this.opts.height,
        frameRate: this.opts.fps,
      },
      audio: this.opts.shareAudio,
    });

    const track = this.stream.getVideoTracks()[0];
    if (!track) {
      throw new Error("No video track in display media");
    }
    track.addEventListener("ended", () => {
      this.opts.onCaptureEnded?.();
      this.stop().catch(() => {});
    });

    const codecString = webCodecsStringForCodec(
      this.codec,
      this.opts.width,
      this.opts.height,
      this.opts.fps,
    );
    const encoderConfig: VideoEncoderConfig = {
      codec: codecString,
      width: this.opts.width,
      height: this.opts.height,
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
      throw new Error(
        `Encoder config not supported: codec=${codecString} ${this.opts.width}x${this.opts.height}@${this.opts.fps} ${this.opts.bitrateKbps}kbps`,
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
            this.description =
              desc instanceof ArrayBuffer
                ? new Uint8Array(desc)
                : new Uint8Array(desc.buffer.slice(0));
          }

          const data = new Uint8Array(chunk.byteLength);
          chunk.copyTo(data);
          const isKey = chunk.type === "key";

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

    // Pump frames from the capture track through the encoder. Uses
    // MediaStreamTrackProcessor where available (Chromium ≥ 94 with
    // the Insertable Streams API). The processor returns VideoFrames
    // that may be GPU-backed; passing them straight to encoder.encode
    // lets Chromium keep the path zero-copy on hardware paths.
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

    void this.pumpLoop();
  }

  private async pumpLoop(): Promise<void> {
    if (!this.reader || !this.encoder) return;
    try {
      while (!this.stopping) {
        const { value: frame, done } = await this.reader.read();
        if (done) break;
        if (!frame) continue;
        try {
          // Encoder backpressure protection: drop frames if the
          // queue is too deep. WebCodecs's queueing is bounded but
          // dropping non-keyframes preemptively keeps latency low.
          const encodeOpts: VideoEncoderEncodeOptions = {};
          if (this.wantKeyframe) {
            encodeOpts.keyFrame = true;
            this.wantKeyframe = false;
          }
          if (this.encoder.encodeQueueSize < 4) {
            this.encoder.encode(frame, encodeOpts);
          }
        } finally {
          frame.close();
        }
        this.frameCounter += 1;
      }
    } catch (e) {
      if (!this.stopping) {
        console.error("[StreamCapture] pump loop error:", e);
      }
    }
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
    case VideoCodec.AV1:
      // AV1 Main profile. Level chosen by frame size: 4.0 = 1080p,
      // 5.0 = 1440p, 5.1 = 4K30, 5.2 = 4K60. The trailing `.08` is bit
      // depth (8-bit). All resolutions Decibell offers fit ≤ 5.2.
      if (width * height > 3840 * 2160) return "av01.0.10M.08"; // 6.0
      if (width * height >= 3840 * 2160 && fps > 30) return "av01.0.09M.08"; // 5.2
      if (width * height >= 3840 * 2160) return "av01.0.08M.08"; // 5.1
      if (width * height >= 2560 * 1440) return "av01.0.06M.08"; // 5.0
      return "av01.0.04M.08"; // 4.0 — 1080p
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
      if (width * height >= 1280 * 720 && fps > 30) return "avc1.640021"; // 3.2
      return "avc1.64001F"; // 3.1 — 720p
    }
  }
}
