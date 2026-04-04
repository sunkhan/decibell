import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useVoiceStore } from "../../stores/voiceStore";

export function saveSettings() {
  const { voiceThresholdDb, streamStereo, inputDevice, outputDevice, separateStreamOutput, streamOutputDevice } = useUiStore.getState();
  const { friendsOnlyDms } = useDmStore.getState();
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  invoke("save_settings", {
    settings: {
      friends_only_dms: friendsOnlyDms,
      voice_threshold_db: voiceThresholdDb,
      stream_stereo: streamStereo,
      input_device: inputDevice,
      output_device: outputDevice,
      separate_stream_output: separateStreamOutput,
      stream_output_device: streamOutputDevice,
      user_volumes: userVolumes,
      local_muted_users: [...localMutedUsers],
    },
  }).catch(console.error);
}
