// Push the persisted voice preferences that a freshly started native
// VoiceEngine does NOT pick up from config on its own: the VAD threshold,
// the DSP toggles, and the separate-stream output routing. Shared by the
// community join flow (joinVoiceChannel) and the P2P DM-call flow
// (call_connected) so both sessions sound identical.
//
// Deliberately NOT re-sent: the input/output device. The engine already
// honoured the saved devices at start (native reads the same config we
// flush before starting it); re-sending hot-swapped the CPAL streams for
// nothing (an audible pop), and passing `null` for a "Default" device trips
// napi's `Option<String>`, which accepts `undefined` but rejects `null`.

import { invoke } from "../../../lib/ipc";
import { useUiStore } from "../../../stores/uiStore";

export async function applyVoicePrefs(): Promise<void> {
  const {
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

  if (separateStreamOutput) {
    // Order matters: enabling separate-stream output rebuilds the voice output
    // on the *default* device, so the chosen voice output device must be
    // (re)applied AFTER the split is configured (audio backlog #2). Await the
    // split so its control message is enqueued before the device sets. Use
    // `?? undefined` (never null) for the Option<String> device args.
    await invoke("set_separate_stream_output", {
      enabled: true,
      device: streamOutputDevice ?? undefined,
    }).catch(console.error);
    invoke("set_output_device", { name: outputDevice ?? undefined }).catch(console.error);
    invoke("set_stream_output_device", { name: streamOutputDevice ?? undefined }).catch(
      console.error,
    );
  }

  invoke("set_aec_enabled", { enabled: aecEnabled }).catch(console.error);
  invoke("set_noise_suppression_level", {
    level: noiseSuppressionLevel,
  }).catch(console.error);
  invoke("set_agc_enabled", { enabled: agcEnabled }).catch(console.error);
}
