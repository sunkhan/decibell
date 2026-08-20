// App-global audio hardware watcher. Mounted once (in AppRoutes).
//
// Keeps `audioDevicesStore` populated so every device picker shows the current
// hardware, refreshing whenever the OS reports a hotplug/unplug via the browser
// `devicechange` event.
//
// It deliberately does NOT re-push the device selection to the live voice
// pipeline on every change. An earlier version did, but wireless headsets
// (and PipeWire graph churn — e.g. a stream's null-sink appearing) fire
// `devicechange` frequently, and each re-push hot-swapped the CPAL streams
// mid-call, producing an audible pop each time. The native pipeline keeps
// running on its chosen device; the user re-selects explicitly from Settings if
// they want to switch.

import { useEffect } from "react";
import { useAudioDevicesStore } from "../stores/audioDevicesStore";

// devicechange can fire several times in a burst (e.g. a USB dock exposing mic
// + speakers). Collapse a burst into a single list refresh.
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
      }, DEBOUNCE_MS);
    };

    md.addEventListener("devicechange", onChange);
    return () => {
      if (timer) clearTimeout(timer);
      md.removeEventListener("devicechange", onChange);
    };
  }, []);
}
