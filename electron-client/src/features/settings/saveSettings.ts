// Settings persistence — call this after a user-facing setting
// changes so the value sticks across restarts. The native
// `save_settings` command takes the full AppSettings blob and writes
// it to ~/.config/<app>/config.json (preserving any existing
// encrypted credentials). We reconstruct the blob from the relevant
// zustand stores on each call; the renderer is the source of truth
// for the live values.
//
// Field names map to the snake_case keys in `native/src/config.rs`'s
// `AppSettings` struct because serde deserialises the JSON we pass
// straight into that struct. Anything mis-named is silently dropped
// by serde and lost on next reload.
//
// All writes are debounced — sliders (bitrate, volume) and wheel
// scrubs fire saveSettings on every tick, which used to mean ~60
// disk writes per second while the user dragged. The trailing
// debounce collapses that into one write 250ms after the user stops
// fiddling. Single-click toggles (checkboxes, dropdowns) get the
// same delay, which is imperceptible.

import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useVoiceStore } from "../../stores/voiceStore";

const DEBOUNCE_MS = 250;
let pendingTimer: ReturnType<typeof setTimeout> | null = null;

function doSave(): void {
  const ui = useUiStore.getState();
  const { friendsOnlyDms } = useDmStore.getState();
  const { userVolumes, localMutedUsers, streamSettings } = useVoiceStore.getState();

  // napi-rs binds a single positional argument of type
  // `serde_json::Value` directly from the JS arg — the param name on
  // the Rust side ("settings") is just a local variable, not a JS
  // object key. Wrapping in `{ settings: ... }` would land
  // `serde_json::from_value` looking for AppSettings fields at the
  // top level of `{settings: {...}}` and finding none, silently
  // populating with defaults instead. So we send the AppSettings
  // fields directly as the args object.
  invoke("save_settings", {
    friends_only_dms: friendsOnlyDms,
    voice_threshold_db: ui.voiceThresholdDb,
    stream_stereo: ui.streamStereo,
    input_device: ui.inputDevice,
    output_device: ui.outputDevice,
    separate_stream_output: ui.separateStreamOutput,
    stream_output_device: ui.streamOutputDevice,
    user_volumes: userVolumes,
    local_muted_users: [...localMutedUsers],
    aec_enabled: ui.aecEnabled,
    noise_suppression_level: ui.noiseSuppressionLevel,
    agc_enabled: ui.agcEnabled,
    upload_limit_bps: ui.uploadLimitBps,
    download_limit_bps: ui.downloadLimitBps,
    channel_cache_size: ui.channelCacheSize,
    media_audio_volume: ui.mediaAudioVolume,
    media_audio_muted: ui.mediaAudioMuted,
    media_video_volume: ui.mediaVideoVolume,
    media_video_muted: ui.mediaVideoMuted,
    stream_resolution: streamSettings.resolution,
    stream_fps: streamSettings.fps,
    stream_quality: streamSettings.quality,
    stream_video_bitrate_kbps: streamSettings.videoBitrateKbps,
    stream_share_audio: streamSettings.shareAudio,
    stream_audio_bitrate_kbps: streamSettings.audioBitrateKbps,
    stream_enforced_codec: streamSettings.enforcedCodec,
    crash_reporting_enabled: ui.crashReportingEnabled,
    crash_reporting_install_id: ui.crashReportingInstallId,
    crash_reporting_consent_shown: ui.crashReportingConsentShown,
  }).catch((e) => console.error("[saveSettings] failed:", e));
}

export function saveSettings(): void {
  if (pendingTimer !== null) {
    clearTimeout(pendingTimer);
  }
  pendingTimer = setTimeout(() => {
    pendingTimer = null;
    doSave();
  }, DEBOUNCE_MS);
}

/// Force any pending debounced save to flush immediately. Use before
/// teardown / window close paths so the user's most recent slider tick
/// isn't lost in the 250ms gap between change and persist.
export function flushSaveSettings(): void {
  if (pendingTimer === null) return;
  clearTimeout(pendingTimer);
  pendingTimer = null;
  doSave();
}
