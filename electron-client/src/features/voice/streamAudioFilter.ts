// The one place that owns "persist the stream-audio app filter AND poke the
// live capture". The Go Live dialog and the live popover both go through
// these helpers: mode + ticked app identities live in
// voiceStore.streamSettings (persisted by setStreamSettings), and while a
// stream is live the same change is pushed to native so the running capture
// re-plans immediately (PipeWire re-links / WASAPI clients re-planned).

import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import type { StreamAudioMode } from "../../types";
import { isNativeEncodeActive } from "../../utils/encoderProbe";

/// Whether this session can pick stream-audio apps at all. Stream audio only
/// exists on the native capture path (Windows always; Linux with a hardware
/// encoder) — macOS and the renderer-WebCodecs fallback stream without audio,
/// so the picker has nothing to control there.
export function canPickStreamAudioApps(): boolean {
  return window.decibell.platform !== "darwin" && isNativeEncodeActive();
}

/// Canonical identity spelling, mirroring native `normalize_identity`:
/// trimmed, lowercase, no trailing ".exe".
export function normalizeAppId(raw: string): string {
  let s = raw.trim().toLowerCase();
  if (s.endsWith(".exe")) s = s.slice(0, -4);
  return s;
}

function pushLiveFilter(): void {
  const { isStreaming, streamSettings } = useVoiceStore.getState();
  if (!isStreaming) return;
  invoke("set_stream_audio_filter", {
    mode: streamSettings.audioMode,
    apps: streamSettings.audioApps,
  }).catch((e) => console.error("[streamAudio] set filter failed:", e));
}

export function setStreamAudioMode(mode: StreamAudioMode): void {
  useVoiceStore.getState().setStreamSettings({ audioMode: mode });
  pushLiveFilter();
}

export function toggleStreamAudioApp(id: string): void {
  const norm = normalizeAppId(id);
  if (!norm) return;
  const { audioApps } = useVoiceStore.getState().streamSettings;
  const next = audioApps.includes(norm)
    ? audioApps.filter((a) => a !== norm)
    : [...audioApps, norm];
  useVoiceStore.getState().setStreamSettings({ audioApps: next });
  pushLiveFilter();
}
