import { useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";

/**
 * Keeps `voiceStore.streamThumbnails` filled for the voice view's stream
 * tiles.
 *
 * Pushed: thumbnails arrive on the dedicated binary IPC channel
 * (`window.decibell.streamThumbnails.subscribe`). The streamer's pipeline
 * makes one at stream start, a follow-up ~2 s later, then every 15 s
 * (thumbnailConfig.ts); the community server relays each to the whole voice
 * channel. Thumbnails for streams we are watching are kept too: when we
 * stop watching, the tile falls back to the poster, and a poster from before
 * we started watching could be minutes old. The cost is one blob URL per
 * stream every 15 s.
 *
 * Pulled: at a 15 s cadence, someone who joins the channel (or opens the
 * voice view: thumbnails are only stored while it's mounted) would stare at
 * an avatar until the next push. So any active stream without a thumbnail
 * gets one fetch of the server's cached copy (FETCH_STREAM_THUMBNAIL, the
 * same request the profile popup uses), once per stream per channel visit.
 *
 * The channel hands us raw JPEG bytes — we wrap them in a
 * `URL.createObjectURL(new Blob(...))` and stash that URL on the voice
 * store. The store revokes the previous URL before replacing it, so blob
 * storage stays bounded.
 */
export function useStreamThumbnails() {
  useEffect(() => {
    const unsubscribe = window.decibell.streamThumbnails.subscribe((thumb) => {
      const state = useVoiceStore.getState();
      const { ownerUsername, data } = thumb;
      // Skip if the stream has already gone away in the time it took
      // the thumbnail to land (race vs. a stop_streaming presence
      // update). Avoids briefly stashing a blob URL that nothing
      // would ever revoke.
      if (!state.activeStreams.some((s) => s.ownerUsername === ownerUsername)) {
        return;
      }
      const blob = new Blob([data], { type: "image/jpeg" });
      state.setStreamThumbnail(ownerUsername, URL.createObjectURL(blob));
    });

    // streamIds already fetched on demand during this channel visit.
    const requested = new Set<string>();
    const fetchMissing = () => {
      const s = useVoiceStore.getState();
      const serverId = s.connectedServerId;
      if (!serverId || !s.connectedChannelId) return;
      for (const stream of s.activeStreams) {
        const owner = stream.ownerUsername;
        if (s.streamThumbnails[owner] || requested.has(stream.streamId)) continue;
        requested.add(stream.streamId);
        invoke("fetch_stream_thumbnail", { serverId, username: owner })
          .then((res) => {
            const jpeg = (res as { jpeg?: Uint8Array } | null)?.jpeg;
            if (!jpeg || jpeg.byteLength === 0) return; // nothing cached yet — a push follows
            const now = useVoiceStore.getState();
            // A push may have landed while we waited; it's at least as new.
            if (now.streamThumbnails[owner]) return;
            if (!now.activeStreams.some((st) => st.streamId === stream.streamId)) return;
            const blob = new Blob([jpeg as BlobPart], { type: "image/jpeg" });
            now.setStreamThumbnail(owner, URL.createObjectURL(blob));
          })
          .catch(() => {});
      }
    };
    fetchMissing();
    const unsubscribeStore = useVoiceStore.subscribe((state, prev) => {
      if (state.connectedChannelId !== prev.connectedChannelId) requested.clear();
      if (
        state.activeStreams !== prev.activeStreams ||
        state.connectedChannelId !== prev.connectedChannelId
      ) {
        fetchMissing();
      }
    });

    return () => {
      unsubscribe();
      unsubscribeStore();
    };
    // Subscribe once for the voice view's lifetime. The callbacks read
    // fresh state via getState(), so nothing here needs to be a dependency.
  }, []);
}
