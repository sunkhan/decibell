import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import type { StreamInfo } from "../../types";

/** Convert dB to linear gain: 10^(dB/20), with -40 dB floor mapped to 0. */
function dbToGain(db: number): number {
  if (db <= -40) return 0;
  return Math.pow(10, db / 20);
}

export function useVoiceEvents() {
  const setSpeaking = useVoiceStore((s) => s.setSpeaking);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const setDeafened = useVoiceStore((s) => s.setDeafened);
  const setLatency = useVoiceStore((s) => s.setLatency);
  const setError = useVoiceStore((s) => s.setError);
  const setParticipants = useVoiceStore((s) => s.setParticipants);
  const setChannelPresence = useVoiceStore((s) => s.setChannelPresence);
  const setUserState = useVoiceStore((s) => s.setUserState);
  const username = useAuthStore((s) => s.username);

  useEffect(() => {
    const promises: Promise<() => void>[] = [];

    promises.push(listen<{ serverId: string; channelId: string; participants: string[]; userStates: { username: string; isMuted: boolean; isDeafened: boolean }[] }>(
      "voice_presence_updated",
      (event) => {
        const { channelId, participants, userStates } = event.payload;
        // Always update per-channel presence map with user states
        setChannelPresence(channelId, participants, userStates);

        // Update connected channel participants
        const connectedId = useVoiceStore.getState().connectedChannelId;
        if (channelId === connectedId) {
          const stateMap = new Map(userStates?.map((s) => [s.username, s]) ?? []);
          setParticipants(
            participants.map((u) => ({
              username: u,
              isMuted: stateMap.get(u)?.isMuted ?? false,
              isDeafened: stateMap.get(u)?.isDeafened ?? false,
              isSpeaking: useVoiceStore.getState().speakingUsers.includes(u),
              audioLevel: 0,
            }))
          );

          // Apply saved per-user volume/mute settings for each recognized peer
          const { userVolumes, localMutedUsers } = useVoiceStore.getState();
          for (const user of participants) {
            const hasCustomVolume = user in userVolumes;
            const isMuted = localMutedUsers.has(user);
            if (hasCustomVolume || isMuted) {
              const db = userVolumes[user] ?? 0;
              const gain = isMuted ? 0 : dbToGain(db);
              invoke("set_user_volume", { username: user, gain }).catch(console.error);
            }
          }
        }
      }
    ));

    promises.push(listen<{ username: string; speaking: boolean }>(
      "voice_user_speaking",
      (event) => {
        const speakingUsername =
          event.payload.username === "__local__"
            ? username ?? ""
            : event.payload.username;
        if (speakingUsername) {
          setSpeaking(speakingUsername, event.payload.speaking);
        }
      }
    ));

    promises.push(listen<{ isMuted: boolean; isDeafened: boolean }>(
      "voice_state_changed",
      (event) => {
        setMuted(event.payload.isMuted);
        setDeafened(event.payload.isDeafened);
      }
    ));

    promises.push(listen<{ username: string; isMuted: boolean; isDeafened: boolean }>(
      "voice_user_state_changed",
      (event) => {
        setUserState(event.payload.username, event.payload.isMuted, event.payload.isDeafened);
      }
    ));

    promises.push(listen<{ latencyMs: number }>("voice_ping_updated", (event) => {
      setLatency(event.payload.latencyMs);
    }));

    promises.push(listen<{ message: string }>("voice_error", (event) => {
      setError(event.payload.message);
    }));

    // Window closed while streaming → auto-stop
    promises.push(listen("stream_capture_ended", () => {
      const { connectedServerId, connectedChannelId, isStreaming, fullscreenStream, isStreamFullscreen } = useVoiceStore.getState();
      if (isStreaming && connectedServerId && connectedChannelId) {
        invoke("stop_screen_share", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
        }).catch(console.error);
        useVoiceStore.getState().setIsStreaming(false);

        // If we were watching our own stream in fullscreen, exit immediately
        // rather than waiting for the server's stream_presence_updated.
        if (fullscreenStream === username && isStreamFullscreen) {
          useVoiceStore.getState().setFullscreenStream(null);
          useVoiceStore.getState().setStreamFullscreen(false);
          getCurrentWindow().setFullscreen(false).catch(() => {});
        }
      }
    }));

    promises.push(listen<{ streams: { streamId: string; ownerUsername: string; hasAudio: boolean; resolutionWidth: number; resolutionHeight: number; fps: number }[] }>(
      "stream_presence_updated",
      (event) => {
        const mapped: StreamInfo[] = event.payload.streams.map((s) => ({
          streamId: s.streamId,
          ownerUsername: s.ownerUsername,
          hasAudio: s.hasAudio,
          resolutionWidth: s.resolutionWidth || 0,
          resolutionHeight: s.resolutionHeight || 0,
          fps: s.fps || 0,
        }));
        useVoiceStore.getState().setActiveStreams(mapped);

        // Remove any watched streams that are no longer active
        const { watchingStreams, fullscreenStream } = useVoiceStore.getState();
        for (const w of watchingStreams) {
          if (!mapped.some((s) => s.ownerUsername === w)) {
            useVoiceStore.getState().removeWatching(w);
          }
        }
        if (fullscreenStream && !mapped.some((s) => s.ownerUsername === fullscreenStream)) {
          useVoiceStore.getState().setFullscreenStream(null);
          if (useVoiceStore.getState().isStreamFullscreen) {
            useVoiceStore.getState().setStreamFullscreen(false);
            getCurrentWindow().setFullscreen(false).catch(() => {});
          }
        }
      }
    ));

    return () => {
      for (const p of promises) {
        p.then((fn) => fn());
      }
    };
  }, [username, setSpeaking, setMuted, setDeafened, setLatency, setError, setParticipants, setChannelPresence, setUserState]);
}
