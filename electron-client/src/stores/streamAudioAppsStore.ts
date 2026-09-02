// Live roster of applications with an audio output, for the stream-audio
// app picker. Populated from the native `list_stream_audio_apps` command
// (PipeWire nodes on Linux, WASAPI sessions on Windows); the picker polls
// `refresh()` every 2 s while mounted, and the store skips the state write
// when the list is unchanged so a poll doesn't re-render anything.
// `supported` is false where per-app stream audio doesn't exist (macOS).

import { create } from "zustand";
import { invoke } from "../lib/ipc";
import type { StreamAudioApp, StreamAudioAppList } from "../types";

interface StreamAudioAppsState {
  apps: StreamAudioApp[];
  supported: boolean;
  /** Re-enumerate from native. A refresh already in flight is reused. */
  refresh: (sourceId?: string) => Promise<void>;
}

let inFlight: Promise<void> | null = null;

function sameList(a: StreamAudioApp[], b: StreamAudioApp[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.id !== y.id ||
      x.name !== y.name ||
      x.active !== y.active ||
      x.ownsWindowSource !== y.ownsWindowSource
    ) {
      return false;
    }
  }
  return true;
}

export const useStreamAudioAppsStore = create<StreamAudioAppsState>((set, get) => ({
  apps: [],
  supported: true,
  refresh: (sourceId) => {
    if (inFlight) return inFlight;
    // `sourceId: undefined` (never null) — napi's Option<String> rejects null.
    inFlight = invoke<StreamAudioAppList>("list_stream_audio_apps", { sourceId })
      .then((list) => {
        const { apps, supported } = get();
        if (supported !== list.supported || !sameList(apps, list.apps)) {
          set({ apps: list.apps, supported: list.supported });
        }
      })
      .catch((e) => {
        console.error("[streamAudioApps] refresh failed:", e);
      })
      .finally(() => {
        inFlight = null;
      });
    return inFlight;
  },
}));
