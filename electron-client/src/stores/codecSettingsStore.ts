// Zustand store for the Settings → Codecs panel.
//
// Holds the user's codec preference toggles + the most recently fetched
// encode/decode capability lists. All mutating methods round-trip through
// native commands so persistence + outbound UpdateCapabilitiesRequest
// broadcast happen authoritatively in Rust.

import { create } from "zustand";
import { invoke } from "../lib/ipc";
import type { CodecCapability, CodecSettings } from "../types";
import { probeEncoders } from "../utils/encoderProbe";
import { probeDecoders } from "../utils/decoderProbe";

interface CodecSettingsState {
  useAv1: boolean;
  useH265: boolean;
  encodeCaps: CodecCapability[]; // probed encoders, post-toggle filtering
  decodeCaps: CodecCapability[]; // probed decoders
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
      invoke<CodecSettings>("get_codec_settings"),
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
    // Native expects { useAv1, useH265 } as the args object directly
    // (napi-rs binds the JS arg to the CodecSettingsValue struct;
    // wrapping in `{ settings: ... }` was the tauri pattern and
    // doesn't apply here).
    await invoke("set_codec_settings", { useAv1: v, useH265: get().useH265 });
    await get().load();
  },

  setUseH265: async (v: boolean) => {
    set({ useH265: v });
    await invoke("set_codec_settings", { useAv1: get().useAv1, useH265: v });
    await get().load();
  },

  refresh: async () => {
    // PR8: encoder + decoder caps are probed in the renderer via
    // WebCodecs.isConfigSupported (no native FFmpeg path), so refresh
    // means "re-run the probes with force=true and ship the fresh
    // results to native". probeEncoders ships caps to native
    // internally; probeDecoders returns and we ship explicitly.
    set({ loadingRefresh: true });
    try {
      const [encodeCaps, decodeCaps] = await Promise.all([
        probeEncoders(true),
        probeDecoders(true),
      ]);
      await invoke("set_decoder_caps", { decoderCaps: decodeCaps }).catch((e) =>
        console.warn("[codecSettings] set_decoder_caps failed:", e),
      );
      set({ encodeCaps, decodeCaps });
    } finally {
      set({ loadingRefresh: false });
    }
  },
}));
