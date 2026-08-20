import { useEffect } from "react";
import { createPortal } from "react-dom";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import StreamVideoPlayer from "./StreamVideoPlayer";
import { getStreamPipHost, resetStreamPipRect } from "./streamPipHost";

// How long the decoder stays warm while sitting on the streams grid (focused
// then backed out) before it's dropped. Long enough that backing out and
// re-focusing is instant; short enough that idling on the grid doesn't keep a
// video decoding invisibly.
const GRID_IDLE_MS = 20_000;

/// Owns the single, persistent stream player. Renders StreamVideoPlayer exactly
/// once — via a portal into the shared host node (see streamPipHost.ts) — for as
/// long as a stream is "loaded" (pipStream). The full view (StreamViewPanel) and
/// the floating mini player each reparent that same host into their own slot, so
/// the decoder is never torn down as the user moves between views OR backs out
/// to the streams grid. Mounted once at the app root so it outlives every view.
export default function StreamPipManager() {
  const activeView = useUiStore((s) => s.activeView);
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const pipStream = useVoiceStore((s) => s.pipStream);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const setPipStream = useVoiceStore((s) => s.setPipStream);

  // Focusing a stream loads it into the persistent player.
  useEffect(() => {
    if (fullscreenStream) setPipStream(fullscreenStream);
  }, [fullscreenStream, setPipStream]);

  // Drop the loaded stream once it's no longer watched or no longer live, so the
  // decoder is torn down instead of leaking.
  useEffect(() => {
    if (
      pipStream &&
      !(
        watchingStreams.includes(pipStream) &&
        activeStreams.some((s) => s.ownerUsername === pipStream)
      )
    ) {
      setPipStream(null);
    }
  }, [pipStream, watchingStreams, activeStreams, setPipStream]);

  // Bound the warm-decoder cost: while sitting on the streams grid (in the voice
  // view, nothing focused) with a stream still loaded, drop it after a short
  // idle. Refocusing or leaving the view cancels the timer, keeping those
  // transitions seamless.
  useEffect(() => {
    const idleOnGrid =
      activeView === "voice" && !fullscreenStream && pipStream != null;
    if (!idleOnGrid) return;
    const t = setTimeout(() => setPipStream(null), GRID_IDLE_MS);
    return () => clearTimeout(t);
  }, [activeView, fullscreenStream, pipStream, setPipStream]);

  // Forget the last on-screen rect when nothing is loaded, so the next stream
  // doesn't morph in from where the old one sat.
  useEffect(() => {
    if (!pipStream) resetStreamPipRect();
  }, [pipStream]);

  const live =
    pipStream != null &&
    watchingStreams.includes(pipStream) &&
    activeStreams.some((s) => s.ownerUsername === pipStream);

  if (!live || !pipStream) return null;

  // key on the streamer so switching to a *different* stream gets a fresh
  // decoder; staying on the same one keeps the instance mounted across views
  // and across the streams grid.
  return createPortal(
    <StreamVideoPlayer
      key={pipStream}
      streamerUsername={pipStream}
      className="h-full w-full object-contain"
    />,
    getStreamPipHost(),
  );
}
