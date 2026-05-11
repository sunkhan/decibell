import { useEffect } from "react";
import { useActiveAudioStore } from "../../stores/activeAudioStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { bindAudioElement } from "./audioController";
import { saveSettings } from "../settings/saveSettings";
import { getCachedAudio, updateCachedAudioState } from "./audioPlaybackState";

// App-level <audio> host. Survives Virtuoso row unmounts so audio
// keeps playing as the user scrolls through the channel. Mirrors
// element events into useActiveAudioStore so the chat-side controls
// re-render. Channel switch clears the active audio (see
// audioPlaybackState.ts's channel-switch listener).
//
// Tauri-era equivalent also unlinked a temp file on swap; PR8's
// Electron build serves attachments through Chromium's HTTP cache
// via `decibell-attachment://`, so there's nothing to clean up.

export default function PersistentAudioLayer() {
  const active = useActiveAudioStore((s) => s.active);
  const setActive = useActiveAudioStore((s) => s.setActive);
  const setPlaybackState = useActiveAudioStore((s) => s.setPlaybackState);

  // Stop audio on channel switch — matches the video-layer policy.
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
          // Seed the fresh element with the user's persisted volume +
          // mute. Lives in uiStore so it survives restarts and tracks
          // the same write path as every other persisted setting.
          const s = useUiStore.getState();
          el.volume = s.mediaAudioVolume;
          el.muted = s.mediaAudioMuted;
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
        updateCachedAudioState(
          active.channelId,
          active.attachmentId,
          e.currentTarget.currentTime,
        );
      }}
      onLoadedMetadata={(e) => {
        const el = e.currentTarget;
        setPlaybackState({ duration: el.duration || 0 });
        // Resume from the cached lastTime if the user previously
        // paused this attachment mid-playback.
        const cached = getCachedAudio(active.channelId, active.attachmentId);
        if (cached && cached.lastTime > 0 && cached.lastTime < el.duration) {
          el.currentTime = cached.lastTime;
        }
      }}
      onDurationChange={(e) =>
        setPlaybackState({ duration: e.currentTarget.duration || 0 })
      }
      onVolumeChange={(e) => {
        // Mirror element → uiStore so any chat-side slider re-renders
        // with the live value, and persist so the level survives
        // restarts. Skip the save when nothing actually changed
        // (the seeding write above fires this once on mount).
        const ui = useUiStore.getState();
        const v = e.currentTarget.volume;
        const m = e.currentTarget.muted;
        if (v !== ui.mediaAudioVolume || m !== ui.mediaAudioMuted) {
          ui.setMediaAudioVolume(v);
          ui.setMediaAudioMuted(m);
          saveSettings();
        }
      }}
    />
  );
}
