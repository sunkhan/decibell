// Zustand store for the Settings → Codecs panel.
//
// Holds the user's codec preference toggles + the most recently fetched
// encode/decode capability lists. Wraps the Tauri commands so the UI
// can stay declarative.
//
// All methods that mutate state go through Tauri commands (set_codec_settings,
// set_decoder_caps, refresh_caps) so the persistence and broadcast happen
// authoritatively in Rust.

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { CodecCapability } from "../types";

interface CodecSettingsState {
  useAv1: boolean;
  useH265: boolean;
  encodeCaps: CodecCapability[];   // probed encoders, post-toggle filtering
  decodeCaps: CodecCapability[];   // probed decoders
  loaded: boolean;
  loadingRefresh: boolean;

  load: () => Promise<void>;
  setUseAv1: (v: boolean) => Promise<void>;
  setUseH265: (v: boolean) => Promise<void>;
  refresh: () => Promise<void>;
}

export const useCodecSettingsStore = create<CodecSettingsState>((set, get) => ({
  useAv1: true,
  useH265: true,
  encodeCaps: [],
  decodeCaps: [],
  loaded: false,
  loadingRefresh: false,

  load: async () => {
    const [settings, caps] = await Promise.all([
      invoke<{ useAv1: boolean; useH265: boolean }>("get_codec_settings"),
      invoke<{ encode: CodecCapability[]; decode: CodecCapability[] }>("get_caps"),
    ]);
    set({
      useAv1: settings.useAv1,
      useH265: settings.useH265,
      encodeCaps: caps.encode,
      decodeCaps: caps.decode,
      loaded: true,
    });
  },

  setUseAv1: async (v: boolean) => {
    set({ useAv1: v });
    await invoke("set_codec_settings", {
      settings: { useAv1: v, useH265: get().useH265 },
    });
    await get().load(); // re-fetch so encodeCaps reflects the new filter
  },

  setUseH265: async (v: boolean) => {
    set({ useH265: v });
    await invoke("set_codec_settings", {
      settings: { useAv1: get().useAv1, useH265: v },
    });
    await get().load();
  },

  refresh: async () => {
    set({ loadingRefresh: true });
    try {
      const caps = await invoke<{ encode: CodecCapability[]; decode: CodecCapability[] }>("refresh_caps");
      set({ encodeCaps: caps.encode, decodeCaps: caps.decode });
    } finally {
      set({ loadingRefresh: false });
    }
  },
}));
