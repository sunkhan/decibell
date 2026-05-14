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
// On engine failure: resets voiceStore state and re-throws so the
// caller can surface the error.

import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { useUiStore } from "../../../stores/uiStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { playSound } from "../../../utils/sounds";

export async function joinVoiceChannel(
  serverId: string,
  channelId: string,
): Promise<void> {
  // Optimistic update — sidebar reflects the pending-join immediately.
  playSound("connect");
  useVoiceStore.getState().setConnectedChannel(serverId, channelId);

  const channel = useChatStore
    .getState()
    .channelsByServer[serverId]?.find((ch) => ch.id === channelId);

  try {
    await invoke("join_voice_channel", {
      serverId,
      channelId,
      voiceBitrateKbps: channel?.voiceBitrateKbps ?? null,
    });
  } catch (err) {
    useVoiceStore.getState().disconnect();
    throw err;
  }

  // Re-apply persisted audio preferences against the fresh pipeline.
  // Saved threshold + AEC/NS/AGC + device picks all live in uiStore.
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
  if (inputDevice) {
    invoke("set_input_device", { name: inputDevice }).catch(console.error);
  }
  if (outputDevice) {
    invoke("set_output_device", { name: outputDevice }).catch(console.error);
  }
  if (separateStreamOutput) {
    invoke("set_separate_stream_output", {
      enabled: true,
      device: streamOutputDevice,
    }).catch(console.error);
  }
  if (aecEnabled) {
    invoke("set_aec_enabled", { enabled: true }).catch(console.error);
  }
  if (noiseSuppressionLevel > 0) {
    invoke("set_noise_suppression_level", { level: noiseSuppressionLevel }).catch(
      console.error,
    );
  }
  if (agcEnabled) {
    invoke("set_agc_enabled", { enabled: true }).catch(console.error);
  }
}
