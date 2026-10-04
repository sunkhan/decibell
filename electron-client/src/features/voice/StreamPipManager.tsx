import { useEffect } from "react";
import { createPortal } from "react-dom";
import { useVoiceStore } from "../../stores/voiceStore";
import StreamVideoPlayer from "./StreamVideoPlayer";
import { getStreamPipHost, resetStreamPipRect } from "./streamPipHost";

/// Owns the single, persistent stream player. Renders StreamVideoPlayer exactly
/// once — via a portal into the shared host node (see streamPipHost.ts) — for as
/// long as a stream is "loaded" (pipStream). The full view (StreamViewPanel) and
/// the floating mini player each reparent that same host into their own slot, so
/// the decoder is never torn down as the user moves between views OR backs out
/// to the streams grid — the grid's tile for that stream claims the host too
/// (StageTiles' StreamTile), so it keeps playing there rather than decoding
/// invisibly. Mounted once at the app root so it outlives every view.
export default function StreamPipManager() {
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

  // No idle drop on the grid any more (there was a 20 s one): the grid shows
  // the loaded stream live in its tile, through this same player, so it is
  // never decoding invisibly there — and dropping it would only swap the tile
  // to a second, cold decoder.

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
