// Shared audio-device roster. Populated from the native `list_audio_devices`
// command (pactl on Linux, cpal elsewhere) and kept fresh by
// `useAudioDeviceSync`, which re-fetches on the browser `devicechange` event.
//
// Previously AudioTab and UserPanel each fetched once on their own mount, so a
// device plugged in mid-session never appeared in either picker. Centralising
// the list here lets a single hotplug listener refresh every consumer at once.

import { create } from "zustand";
import { invoke } from "../lib/ipc";

export interface AudioDevice {
  name: string;
  label: string;
}

interface AudioDevicesState {
  inputs: AudioDevice[];
  outputs: AudioDevice[];
  /** True while a refresh is in flight — used to coalesce concurrent calls. */
  loading: boolean;
  /** Re-enumerate devices from native. Safe to call from many places; a
   *  refresh already in flight is reused instead of spawning a second. */
  refresh: () => Promise<void>;
}

let inFlight: Promise<void> | null = null;

export const useAudioDevicesStore = create<AudioDevicesState>((set) => ({
  inputs: [],
  outputs: [],
  loading: false,
  refresh: () => {
    if (inFlight) return inFlight;
    set({ loading: true });
    inFlight = invoke<{ inputs: AudioDevice[]; outputs: AudioDevice[] }>(
      "list_audio_devices",
    )
      .then((list) => {
        set({ inputs: list.inputs, outputs: list.outputs, loading: false });
      })
      .catch((e) => {
        console.error("[audioDevices] refresh failed:", e);
        set({ loading: false });
      })
      .finally(() => {
        inFlight = null;
      });
    return inFlight;
  },
}));
