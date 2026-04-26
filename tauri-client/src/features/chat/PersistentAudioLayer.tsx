import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useActiveAudioStore } from "../../stores/activeAudioStore";
import { useChatStore } from "../../stores/chatStore";
import { bindAudioElement } from "./audioController";

// App-level <audio> host. Survives Virtuoso row unmounts so audio
// keeps playing as the user scrolls through the channel. Mirrors
// element events into useActiveAudioStore so the chat-side controls
// re-render. Channel switch clears the active audio (matches video
// behaviour) and triggers cleanup of the disk temp file.

export default function PersistentAudioLayer() {
  const active = useActiveAudioStore((s) => s.active);
  const setActive = useActiveAudioStore((s) => s.setActive);
  const setPlaybackState = useActiveAudioStore((s) => s.setPlaybackState);
  // Track the path that's currently on disk so we can unlink the
  // *previous* file when the active changes (or clears).
  const previousTempRef = useRef<string | null>(null);

  // Cleanup the prior temp file whenever active.tempPath changes.
  // Includes the active=null transition (channel switch, replaced).
  useEffect(() => {
    const cur = active?.tempPath ?? null;
    const prev = previousTempRef.current;
    if (prev && prev !== cur) {
      invoke("cleanup_temp_attachment", { path: prev }).catch(() => {});
    }
    previousTempRef.current = cur;
  }, [active?.tempPath]);

  // Stop audio on channel switch — matches the video-layer policy and
  // bounds the temp-file lifetime to a single channel session.
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
      ref={(el) => bindAudioElement(el)}
      src={active.src}
      autoPlay
      preload="auto"
      onPlay={() => setPlaybackState({ playing: true })}
      onPause={() => setPlaybackState({ playing: false })}
      onEnded={() => setPlaybackState({ playing: false })}
      onTimeUpdate={(e) => setPlaybackState({ time: e.currentTarget.currentTime })}
      onLoadedMetadata={(e) => setPlaybackState({ duration: e.currentTarget.duration || 0 })}
      onDurationChange={(e) => setPlaybackState({ duration: e.currentTarget.duration || 0 })}
    />
  );
}
