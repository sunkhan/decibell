// App-global audio hardware watcher. Mounted once (in AppRoutes).
//
// Two jobs:
//   1. Keep `audioDevicesStore` populated so every device picker shows the
//      current hardware, refreshing whenever the OS reports a hotplug/unplug
//      via the browser `devicechange` event.
//   2. When a change lands while we're in a voice channel, re-push the current
//      input/output selection to the native pipeline so it re-resolves the
//      device — picking up a just-plugged headset, or falling back cleanly when
//      the active device disappears instead of leaving a dead cpal stream.

import { useEffect } from "react";
import { invoke } from "../lib/ipc";
import { useAudioDevicesStore } from "../stores/audioDevicesStore";
import { useUiStore } from "../stores/uiStore";
import { useVoiceStore } from "../stores/voiceStore";

// devicechange can fire several times in a burst (e.g. a USB dock exposing
// mic + speakers). Collapse a burst into a single refresh + re-apply.
const DEBOUNCE_MS = 400;

export function useAudioDeviceSync(): void {
  useEffect(() => {
    const md = navigator.mediaDevices;

    // Initial population so pickers aren't empty before the first hotplug.
    useAudioDevicesStore.getState().refresh();

    if (!md?.addEventListener) return;

    let timer: ReturnType<typeof setTimeout> | null = null;

    const onChange = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        useAudioDevicesStore.getState().refresh();

        // Only re-apply to the pipeline if we're actually in a call.
        if (!useVoiceStore.getState().connectedChannelId) return;
        const { inputDevice, outputDevice, separateStreamOutput, streamOutputDevice } =
          useUiStore.getState();
        invoke("set_input_device", { name: inputDevice ?? null }).catch(console.error);
        invoke("set_output_device", { name: outputDevice ?? null }).catch(console.error);
        if (separateStreamOutput) {
          invoke("set_stream_output_device", {
            name: streamOutputDevice ?? null,
          }).catch(console.error);
        }
      }, DEBOUNCE_MS);
    };

    md.addEventListener("devicechange", onChange);
    return () => {
      if (timer) clearTimeout(timer);
      md.removeEventListener("devicechange", onChange);
    };
  }, []);
}
