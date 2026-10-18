// One path for a screen share that ends without the user pressing Stop:
//   • the source went away — the shared window closed, the monitor was
//     unplugged, the desktop's own "stop sharing" (KDE / GNOME tray) closed
//     the screen-cast session. Native capture reports `stream_capture_ended`;
//     the WebCodecs capture sees its track end.
//   • the capture or the encoder broke — native `native_stream_failed`, or
//     StreamCapture's own error paths (which toast for themselves).
//
// Every one lands here, so the UI, the native engine and the community agree
// the stream is over: the LIVE state clears, stopActiveStream() tears the
// session down (its stop_screen_share tells the community, which tells the
// watchers), a DM-call peer gets STREAM_STOP, and the user sees why.

import { invoke } from "../../../lib/ipc";
import { getCurrentWindow } from "../../../lib/window";
import { useAuthStore } from "../../../stores/authStore";
import { toast } from "../../../stores/toastStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { playSound } from "../../../utils/sounds";
import { announceCallStreamStop } from "../../call/callActions";
import { activeStreamCapture, stopActiveStream } from "./StreamCapture";

export type StreamEnd =
  /// The source went away. `notify` toasts it (native capture: the user
  /// may not have caused it); the WebCodecs track end leaves it to the OS.
  | { kind: "source"; notify: boolean }
  /// The capture or encoder broke. `reason` is native's text (logged);
  /// `notify` is false where the caller already toasted.
  | { kind: "failed"; reason?: string; notify: boolean };

export function endStreamFromCapture(end: StreamEnd): void {
  const voice = useVoiceStore.getState();
  const session = activeStreamCapture();
  const wasLive = voice.isStreaming || session !== null;
  if (end.kind === "failed" && end.reason) {
    console.error("[stream] stream failed:", end.reason);
  }

  // stopActiveStream() drives stop_screen_share with the session's ids
  // (native prefers the channel the stream is announced in either way, so
  // a stream carried into another channel is stopped there). Without a
  // renderer session — a reload lost it — stop the native engine directly.
  if (session) {
    stopActiveStream().catch(() => {});
  } else {
    invoke("stop_screen_share", {}).catch(() => {});
  }
  if (!wasLive) return;

  voice.setIsStreaming(false);
  const me = useAuthStore.getState().uid;
  if (voice.fullscreenStream === me && voice.isStreamFullscreen) {
    voice.setFullscreenStream(null);
    voice.setStreamFullscreen(false);
    getCurrentWindow().setFullscreen(false).catch(() => {});
  }
  // In a DM call the peer has no server presence to learn it from
  // (no-op outside a call).
  announceCallStreamStop();
  playSound("stream_stop");

  if (!end.notify) return;
  if (end.kind === "source") {
    toast.info("Stream ended", "The shared screen or window is no longer available.");
  } else {
    toast.error("Stream stopped", "The GPU encoder or screen capture stopped. Try going live again.");
  }
}
