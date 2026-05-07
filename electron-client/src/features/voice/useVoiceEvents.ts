import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { useVoiceStatsStore } from "../../stores/voiceStatsStore";
import {
  VideoCodec,
  type ClientCapabilities,
  type StreamInfo,
  type StreamCodecChangeReason,
} from "../../types";
import { playSound } from "../../utils/sounds";
import { buildCodecToast } from "../../utils/codecToasts";
import { getCurrentWindow } from "../../lib/window";

/// dB → linear gain. -40 dB floor maps to 0 (effectively muted).
function dbToGain(db: number): number {
  if (db <= -40) return 0;
  return Math.pow(10, db / 20);
}

/// Voice-channel event subscriber. Listens for:
///   • voice_presence_updated — channel roster + per-user mute/deafen
///   • voice_user_speaking — RMS speaking detection
///   • voice_state_changed — local mute/deafen confirmation
///   • voice_user_state_changed — peer mute/deafen state echoed by server
///   • voice_ping_updated, voice_connection_stats — telemetry
///   • voice_error — pipeline-emitted errors
///
/// Streaming side: stream_presence_updated, stream_codec_changed,
/// stream_capture_ended, stream_gpu_fallback. Thumbnail events live in
/// useStreamThumbnails so the listener can subscribe/unsubscribe per
/// active-stream count without churning this hook.
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

    promises.push(
      listen<{
        serverId: string;
        channelId: string;
        participants: string[];
        userStates: { username: string; isMuted: boolean; isDeafened: boolean }[];
        userCapabilities?: ClientCapabilities[];
      }>("voice_presence_updated", (event) => {
        const { channelId, participants, userStates, userCapabilities } = event.payload;
        setChannelPresence(channelId, participants, userStates, userCapabilities);

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

        if (channelId === connectedId) {
          const stateMap = new Map(userStates?.map((s) => [s.username, s]) ?? []);
          setParticipants(
            participants.map((u) => ({
              username: u,
              isMuted: stateMap.get(u)?.isMuted ?? false,
              isDeafened: stateMap.get(u)?.isDeafened ?? false,
              isSpeaking: useVoiceStore.getState().speakingUsers.includes(u),
              audioLevel: 0,
            })),
          );

          // Re-apply saved per-user volume / local-mute on every roster
          // change so peers picked up after the user's muted them stay
          // muted, and so volume tweaks survive churn.
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
      }),
    );

    promises.push(
      listen<{ username: string; speaking: boolean }>("voice_user_speaking", (event) => {
        const speakingUsername =
          event.payload.username === "__local__" ? username ?? "" : event.payload.username;
        if (speakingUsername) {
          setSpeaking(speakingUsername, event.payload.speaking);
        }
      }),
    );

    promises.push(
      listen<{ isMuted: boolean; isDeafened: boolean }>("voice_state_changed", (event) => {
        setMuted(event.payload.isMuted);
        setDeafened(event.payload.isDeafened);
      }),
    );

    promises.push(
      listen<{ username: string; isMuted: boolean; isDeafened: boolean }>(
        "voice_user_state_changed",
        (event) => {
          setUserState(event.payload.username, event.payload.isMuted, event.payload.isDeafened);
        },
      ),
    );

    promises.push(
      listen<{ latencyMs: number }>("voice_ping_updated", (event) => {
        setLatency(event.payload.latencyMs);
      }),
    );

    promises.push(
      listen<{ latencyMs: number | null; packetLossPct: number }>(
        "voice_connection_stats",
        (event) => {
          useVoiceStatsStore.getState().pushSample({
            ts: Date.now(),
            pingMs: event.payload.latencyMs,
            lossPct: event.payload.packetLossPct,
          });
        },
      ),
    );

    promises.push(
      listen<{ message: string }>("voice_error", (event) => {
        setError(event.payload.message);
      }),
    );

    // ── Streaming events ──

    // Capture source ended (window closed, monitor disconnected) → auto-stop.
    promises.push(
      listen("stream_capture_ended", () => {
        const {
          connectedServerId,
          connectedChannelId,
          isStreaming,
          fullscreenStream,
          isStreamFullscreen,
        } = useVoiceStore.getState();
        if (isStreaming && connectedServerId && connectedChannelId) {
          invoke("stop_screen_share", {
            serverId: connectedServerId,
            channelId: connectedChannelId,
          }).catch(console.error);
          useVoiceStore.getState().setIsStreaming(false);

          if (fullscreenStream === username && isStreamFullscreen) {
            useVoiceStore.getState().setFullscreenStream(null);
            useVoiceStore.getState().setStreamFullscreen(false);
            getCurrentWindow().setFullscreen(false).catch(() => {});
          }
        }
      }),
    );

    let prevStreamOwners: Set<string> | null = null;

    promises.push(
      listen<{
        streams: {
          streamId: string;
          ownerUsername: string;
          hasAudio: boolean;
          resolutionWidth: number;
          resolutionHeight: number;
          fps: number;
          currentCodec?: number;
          enforcedCodec?: number;
        }[];
      }>("stream_presence_updated", (event) => {
        const mapped: StreamInfo[] = event.payload.streams.map((s) => ({
          streamId: s.streamId,
          ownerUsername: s.ownerUsername,
          hasAudio: s.hasAudio,
          resolutionWidth: s.resolutionWidth || 0,
          resolutionHeight: s.resolutionHeight || 0,
          fps: s.fps || 0,
          currentCodec: (s.currentCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
          enforcedCodec: (s.enforcedCodec ?? VideoCodec.UNKNOWN) as VideoCodec,
        }));

        if (prevStreamOwners) {
          const current = new Set(mapped.map((s) => s.ownerUsername));
          for (const owner of current) {
            if (!prevStreamOwners.has(owner) && owner !== username)
              playSound("stream_start");
          }
          for (const owner of prevStreamOwners) {
            if (!current.has(owner) && owner !== username)
              playSound("stream_stop");
          }
        }
        prevStreamOwners = new Set(mapped.map((s) => s.ownerUsername));

        useVoiceStore.getState().setActiveStreams(mapped);

        const { watchingStreams, fullscreenStream } = useVoiceStore.getState();
        for (const w of watchingStreams) {
          if (!mapped.some((s) => s.ownerUsername === w)) {
            useVoiceStore.getState().removeWatching(w);
          }
        }
        if (
          fullscreenStream &&
          !mapped.some((s) => s.ownerUsername === fullscreenStream)
        ) {
          useVoiceStore.getState().setFullscreenStream(null);
          if (useVoiceStore.getState().isStreamFullscreen) {
            useVoiceStore.getState().setStreamFullscreen(false);
            getCurrentWindow().setFullscreen(false).catch(() => {});
          }
        }
      }),
    );

    promises.push(
      listen<{
        channelId: string;
        streamerUsername: string;
        newCodec: number;
        newWidth: number;
        newHeight: number;
        newFps: number;
        reason: number;
      }>("stream_codec_changed", (event) => {
        const isLocalUserStreamer = event.payload.streamerUsername === username;
        const toast = buildCodecToast(
          {
            channelId: event.payload.channelId,
            streamerUsername: event.payload.streamerUsername,
            newCodec: event.payload.newCodec as VideoCodec,
            newWidth: event.payload.newWidth,
            newHeight: event.payload.newHeight,
            newFps: event.payload.newFps,
            reason: event.payload.reason as StreamCodecChangeReason,
          },
          isLocalUserStreamer,
        );
        // ToastStore lands with the toasts PR; until then surface in console.
        if (toast) {
          console.log("[stream-codec-changed]", toast.text);
        }
      }),
    );

    promises.push(
      listen<{ error: string }>("stream_gpu_fallback", (event) => {
        console.warn(
          "[stream-gpu-fallback] Streaming via CPU path — higher CPU usage:",
          event.payload.error,
        );
      }),
    );

    return () => {
      for (const p of promises) {
        p.then((fn) => fn());
      }
    };
  }, [
    username,
    setSpeaking,
    setMuted,
    setDeafened,
    setLatency,
    setError,
    setParticipants,
    setChannelPresence,
    setUserState,
  ]);
}
