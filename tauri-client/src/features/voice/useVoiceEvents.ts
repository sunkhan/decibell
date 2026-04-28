import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { VideoCodec, type StreamInfo } from "../../types";
import { playSound } from "../../utils/sounds";

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
    let prevParticipants: Set<string> | null = null;

    promises.push(listen<{ serverId: string; channelId: string; participants: string[]; userStates: { username: string; isMuted: boolean; isDeafened: boolean }[]; userCapabilities?: import("../../types").ClientCapabilities[] }>(
      "voice_presence_updated",
      (event) => {
        const { channelId, participants, userStates, userCapabilities } = event.payload;
        // Always update per-channel presence map with user states
        setChannelPresence(channelId, participants, userStates, userCapabilities);

        // Play join/leave sounds for other users in our connected channel
        const connectedId = useVoiceStore.getState().connectedChannelId;
        if (channelId === connectedId && prevParticipants) {
          const current = new Set(participants);
          for (const u of participants) {
            if (!prevParticipants.has(u) && u !== username) playSound("user_join");
          }
          for (const u of prevParticipants) {
            if (!current.has(u) && u !== username) playSound("user_leave");
          }
        }
        if (channelId === connectedId) {
          prevParticipants = new Set(participants);
        }

        // Update connected channel participants
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

    let prevStreamOwners: Set<string> | null = null;

    promises.push(listen<{ streams: { streamId: string; ownerUsername: string; hasAudio: boolean; resolutionWidth: number; resolutionHeight: number; fps: number; currentCodec?: number; enforcedCodec?: number }[] }>(
      "stream_presence_updated",
      (event) => {
        const mapped: StreamInfo[] = event.payload.streams.map((s) => ({
          streamId: s.streamId,
          ownerUsername: s.ownerUsername,
          hasAudio: s.hasAudio,
          resolutionWidth: s.resolutionWidth || 0,
          resolutionHeight: s.resolutionHeight || 0,
          fps: s.fps || 0,
          // Server now populates these (Plan A Group 7); fall back to
          // UNKNOWN if the field is absent (older server build).
          currentCodec: (s.currentCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
          enforcedCodec: (s.enforcedCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
        }));

        // Play stream start/stop sounds for other users
        if (prevStreamOwners) {
          const current = new Set(mapped.map((s) => s.ownerUsername));
          for (const owner of current) {
            if (!prevStreamOwners.has(owner) && owner !== username) playSound("stream_start");
          }
          for (const owner of prevStreamOwners) {
            if (!current.has(owner) && owner !== username) playSound("stream_stop");
          }
        }
        prevStreamOwners = new Set(mapped.map((s) => s.ownerUsername));

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

    // Plan C: codec change toast — fires whenever the streamer renegotiates.
    promises.push(listen<{
      channelId: string;
      streamerUsername: string;
      newCodec: number;
      newWidth: number;
      newHeight: number;
      newFps: number;
      reason: number;
    }>("stream_codec_changed", async (event) => {
      const { buildCodecToast } = await import("../../utils/codecToasts");
      const { useToastStore } = await import("../../stores/toastStore");
      const isLocalUserStreamer = event.payload.streamerUsername === username;
      const toast = buildCodecToast({
        channelId: event.payload.channelId,
        streamerUsername: event.payload.streamerUsername,
        newCodec: event.payload.newCodec as import("../../types").VideoCodec,
        newWidth: event.payload.newWidth,
        newHeight: event.payload.newHeight,
        newFps: event.payload.newFps,
        reason: event.payload.reason as import("../../types").StreamCodecChangeReason,
      }, isLocalUserStreamer);
      if (toast) {
        useToastStore.getState().push({
          severity: "info",
          title: "Stream codec changed",
          body: toast.text,
          duration: 4000,
        });
      }
    }));

    // Plan-D-1: GPU zero-copy pipeline failed at start; we silently fell
    // back to the CPU readback path. Notify the user so they understand
    // why CPU usage is higher than expected.
    promises.push(listen<{ error: string }>("stream_gpu_fallback", async (event) => {
      const { useToastStore } = await import("../../stores/toastStore");
      useToastStore.getState().push({
        severity: "warning",
        title: "GPU encoding unavailable",
        body: `Streaming via CPU path — higher CPU usage. ${event.payload.error}`,
        duration: 6000,
      });
    }));

    return () => {
      for (const p of promises) {
        p.then((fn) => fn());
      }
    };
  }, [username, setSpeaking, setMuted, setDeafened, setLatency, setError, setParticipants, setChannelPresence, setUserState]);
}
