// Shared voice-channel join flow used by ServerChannelsSidebar (when
// the user clicks a voice channel row) and UserProfilePopup (when the
// user clicks a live stream thumbnail).
//
// Responsibilities:
//   1. Play the connect sound + optimistically update voiceStore.connectedChannel
//      so the sidebar shows the pending-join immediately.
//   2. Call `join_voice_channel` via napi with the channel's voice bitrate.
//   3. Re-apply persisted audio device + DSP preferences against the
//      newly-spawned pipeline.
//
// Screen sharing while switching channels: the community server ends your
// stream when you leave a channel, so a naive switch left the client thinking
// it was still streaming (stuck Stop button) with a zombie encoder. This flow
// reconciles that. By default a mid-stream switch ends the stream cleanly. When
// the "Take stream with me when switching voice channels" setting is on (and
// it's a same-server switch) the stream is carried into the new channel instead.
//
// On engine failure: resets voiceStore state and re-throws so the
// caller can surface the error.

import { invoke } from "../../../lib/ipc";
import { useAuthStore } from "../../../stores/authStore";
import { useChatStore } from "../../../stores/chatStore";
import { useUiStore } from "../../../stores/uiStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { playSound } from "../../../utils/sounds";
import { flushSaveSettings } from "../../settings/saveSettings";
import { applyVoicePrefs } from "./applyVoicePrefs";
import { endCall } from "../../call/callActions";

export async function joinVoiceChannel(
  serverId: string,
  channelId: string,
): Promise<void> {
  // A DM call and a voice channel are mutually exclusive (one native
  // VoiceEngine, one mic). Hang up first; native refuses the join otherwise.
  await endCall("Joined a voice channel");

  // Capture the stream state BEFORE the optimistic channel update below.
  const voice = useVoiceStore.getState();
  const prevServerId = voice.connectedServerId;
  const prevChannelId = voice.connectedChannelId;
  const switchingChannel = prevChannelId !== null && prevChannelId !== channelId;
  const wasStreaming = voice.isStreaming && !!prevServerId && !!prevChannelId;
  // "Take stream with me" applies only to a same-server channel switch while
  // streaming, and only when the user opted in. Cross-server moves (and the
  // default-off case) end the stream instead — a UDP media session can't be
  // carried across community servers.
  const takeStreamAlong =
    wasStreaming &&
    switchingChannel &&
    prevServerId === serverId &&
    useUiStore.getState().takeStreamOnChannelSwitch;

  // Stop watching any streams from the OLD channel before switching. The flow
  // previously reconciled only our own stream, so watch subscriptions leaked on
  // a channel switch: the server may keep relaying the old channel's frames to a
  // renderer that no longer has a subscriber, and the watch state never cleared
  // for the old (server, channel). Fire the stops un-awaited (best-effort) and
  // clear the local watch state so the new channel's presence repopulates fresh.
  if (
    switchingChannel &&
    prevServerId &&
    prevChannelId &&
    voice.watchingStreams.length > 0
  ) {
    const ownUsername = useAuthStore.getState().username;
    for (const target of voice.watchingStreams) {
      if (target === ownUsername) continue; // own self-preview isn't a server watch
      invoke("stop_watching", {
        serverId: prevServerId,
        channelId: prevChannelId,
        targetUsername: target,
      }).catch(() => {});
    }
    useVoiceStore.setState({
      watchingStreams: [],
      fullscreenStream: null,
      pipStream: null,
    });
  }

  // Default / cross-server: end the stream cleanly BEFORE the switch, using the
  // OLD channel, so the server, native engine and UI all agree it's over.
  if (wasStreaming && switchingChannel && !takeStreamAlong && prevServerId && prevChannelId) {
    try {
      const { stopActiveStream } = await import("./StreamCapture");
      await stopActiveStream();
      await invoke("stop_screen_share", {
        serverId: prevServerId,
        channelId: prevChannelId,
      }).catch(console.error);
    } finally {
      useVoiceStore.getState().setIsStreaming(false);
    }
  }

  // Optimistic update — sidebar reflects the pending-join immediately.
  playSound("connect");
  useVoiceStore.getState().setConnectedChannel(serverId, channelId);

  const channel = useChatStore
    .getState()
    .channelsByServer[serverId]?.find((ch) => ch.id === channelId);

  // Flush any debounced settings write first: native's VoiceEngine::start
  // reads the on-disk config to pick the initial input/output device, so a
  // device change made <250ms before joining would otherwise be read stale
  // (audio backlog #5).
  flushSaveSettings();

  try {
    await invoke("join_voice_channel", {
      serverId,
      channelId,
      voiceBitrateKbps: channel?.voiceBitrateKbps ?? null,
    });
  } catch (err) {
    // If the join failed while we were streaming (and hadn't already ended the
    // stream above), tear it down too so a disconnected voice state can't leave
    // a zombie encoder running. Idempotent if the stream was already stopped.
    if (wasStreaming) {
      const { stopActiveStream } = await import("./StreamCapture");
      await stopActiveStream().catch(() => {});
      if (prevServerId && prevChannelId) {
        await invoke("stop_screen_share", {
          serverId: prevServerId,
          channelId: prevChannelId,
        }).catch(() => {});
      }
      useVoiceStore.getState().setIsStreaming(false);
    }
    useVoiceStore.getState().disconnect();
    throw err;
  }

  // Apply the preferences the engine does NOT set up at start (VAD threshold,
  // DSP toggles, separate-stream routing) — shared with the DM-call flow.
  await applyVoicePrefs();

  // Carry the stream into the new channel now that the new voice engine (and
  // its media socket) exists. On any failure, end the stream cleanly rather
  // than leaving a zombie encoder / stuck Stop button.
  if (takeStreamAlong) {
    try {
      const { activeStreamCapture } = await import("./StreamCapture");
      const cap = activeStreamCapture();
      if (cap) {
        await cap.retarget(serverId, channelId);
      } else {
        useVoiceStore.getState().setIsStreaming(false);
      }
    } catch (e) {
      console.error("[joinVoiceChannel] failed to carry stream to new channel:", e);
      const { stopActiveStream } = await import("./StreamCapture");
      await stopActiveStream().catch(() => {});
      await invoke("stop_screen_share", { serverId, channelId }).catch(() => {});
      useVoiceStore.getState().setIsStreaming(false);
    }
  }
}
