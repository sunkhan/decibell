import { useEffect } from "react";
import { useActiveAudioStore } from "../../stores/activeAudioStore";
import { useChatStore } from "../../stores/chatStore";
import { bindAudioElement } from "./audioController";
import { getCachedAudio, updateCachedAudioState } from "./tempAudioCache";

// App-level <audio> host. Survives Virtuoso row unmounts so audio
// keeps playing as the user scrolls through the channel. Mirrors
// element events into useActiveAudioStore so the chat-side controls
// re-render. Channel switch clears the active audio (matches video
// behaviour) and triggers cleanup of the disk temp file.

export default function PersistentAudioLayer() {
  const active = useActiveAudioStore((s) => s.active);
  const setActive = useActiveAudioStore((s) => s.setActive);
  const setPlaybackState = useActiveAudioStore((s) => s.setPlaybackState);

  // Stop audio on channel switch — matches the video-layer policy and
  // bounds the temp-file lifetime to a single channel session.
  // tempAudioCache owns the unlinks; we just clear active here so the
  // element drops the now-doomed src before the cache deletes the file.
  useEffect(() => {
    let last = useChatStore.getState().activeChannelId;
    return useChatStore.subscribe((state) => {
      const next = state.activeChannelId;
      if (next === last) return;
      last = next;
      if (useActiveAudioStore.getState().active) {
        setActive(null);
      }
    });
  }, [setActive]);

  if (!active) return null;

  return (
    // eslint-disable-next-line jsx-a11y/media-has-caption
    <audio
      // Re-key by attachment id so switching forces a fresh element
      // (drops any decoder buffers / buffered ranges from the old src).
      key={active.attachmentId}
      ref={(el) => {
        bindAudioElement(el);
        if (el) {
          // Seed the fresh element with the user's preserved volume +
          // mute so swapping attachments doesn't reset their levels.
          const s = useActiveAudioStore.getState();
          el.volume = s.volume;
          el.muted = s.muted;
        }
      }}
      src={active.src}
      autoPlay
      preload="auto"
      onPlay={() => setPlaybackState({ playing: true })}
      onPause={() => setPlaybackState({ playing: false })}
      onEnded={() => setPlaybackState({ playing: false })}
      onTimeUpdate={(e) => {
        setPlaybackState({ time: e.currentTarget.currentTime });
        // Persist position so a re-click on this attachment resumes
        // from where the user paused. Write happens ~4×/sec, which
        // is the timeupdate event's natural cadence — cheap.
        updateCachedAudioState(active.channelId, active.attachmentId, e.currentTarget.currentTime);
      }}
      onLoadedMetadata={(e) => {
        const el = e.currentTarget;
        setPlaybackState({ duration: el.duration || 0 });
        // Resume from the cached lastTime if the user previously
        // paused this attachment mid-playback. Skip when the cached
        // value is past the file's end (defensive — shouldn't happen).
        const cached = getCachedAudio(active.channelId, active.attachmentId);
        if (cached && cached.lastTime > 0 && cached.lastTime < el.duration) {
          el.currentTime = cached.lastTime;
        }
      }}
      onDurationChange={(e) => setPlaybackState({ duration: e.currentTarget.duration || 0 })}
      onVolumeChange={(e) =>
        setPlaybackState({
          volume: e.currentTarget.volume,
          muted: e.currentTarget.muted,
        })
      }
    />
  );
}
