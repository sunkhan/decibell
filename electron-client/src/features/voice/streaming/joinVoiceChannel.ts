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
import { useChatStore } from "../../../stores/chatStore";
import { useUiStore } from "../../../stores/uiStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { playSound } from "../../../utils/sounds";
import { flushSaveSettings } from "../../settings/saveSettings";

export async function joinVoiceChannel(
  serverId: string,
  channelId: string,
): Promise<void> {
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

  // Re-apply persisted audio preferences against the fresh pipeline. The
  // engine already honoured the saved input/output device at start (same
  // config we just flushed), but threshold, DSP toggles and separate-stream
  // routing are not applied at start, so push them here.
  //
  // Send device + DSP values UNCONDITIONALLY (null included): the old
  // truthy-only guards left a "Default" (null) device pick or a disabled
  // toggle stuck on whatever the previous session had set.
  const {
    inputDevice,
    outputDevice,
    separateStreamOutput,
    streamOutputDevice,
    voiceThresholdDb,
    aecEnabled,
    noiseSuppressionLevel,
    agcEnabled,
  } = useUiStore.getState();

  invoke("set_voice_threshold", {
    thresholdDb: voiceThresholdDb <= -60 ? -96 : voiceThresholdDb,
  }).catch(console.error);

  invoke("set_input_device", { name: inputDevice ?? null }).catch(console.error);

  if (separateStreamOutput) {
    // Order matters: enabling separate-stream output rebuilds the voice
    // output on the *default* device, so the chosen voice output device must
    // be (re)applied AFTER the split is configured (audio backlog #2). Await
    // the split so its control message is enqueued before the device sets.
    await invoke("set_separate_stream_output", {
      enabled: true,
      device: streamOutputDevice ?? null,
    }).catch(console.error);
    invoke("set_output_device", { name: outputDevice ?? null }).catch(console.error);
    invoke("set_stream_output_device", { name: streamOutputDevice ?? null }).catch(
      console.error,
    );
  } else {
    invoke("set_output_device", { name: outputDevice ?? null }).catch(console.error);
  }

  invoke("set_aec_enabled", { enabled: aecEnabled }).catch(console.error);
  invoke("set_noise_suppression_level", {
    level: noiseSuppressionLevel,
  }).catch(console.error);
  invoke("set_agc_enabled", { enabled: agcEnabled }).catch(console.error);

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
