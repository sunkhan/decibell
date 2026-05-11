// Read the persisted config blob from disk and hydrate the relevant
// stores. Run once at boot (from main.tsx) before the React tree
// mounts so settings are in place by the time any component reads
// them. If the config doesn't exist, decode fails, or any individual
// field is missing/invalid, we fall back to the in-store defaults
// silently — never abort.
//
// Mirrors the load flow tauri-client did inline in App.tsx, lifted
// into its own module here so main.tsx stays terse.

import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { VideoCodec } from "../../types";

interface LoadedConfigShape {
  credentials: { username: string; password: string } | null;
  settings: {
    friends_only_dms: boolean;
    voice_threshold_db: number | null;
    stream_stereo: boolean;
    input_device: string | null;
    output_device: string | null;
    separate_stream_output: boolean;
    stream_output_device: string | null;
    user_volumes: Record<string, number>;
    local_muted_users: string[];
    aec_enabled: boolean;
    noise_suppression_level: number;
    agc_enabled: boolean;
    upload_limit_bps: number;
    download_limit_bps: number;
    channel_cache_size: number;
    media_audio_volume: number | null;
    media_audio_muted: boolean;
    media_video_volume: number | null;
    media_video_muted: boolean;
    use_av1: boolean;
    use_h265: boolean;
    stream_resolution: string | null;
    stream_fps: number | null;
    stream_quality: string | null;
    stream_video_bitrate_kbps: number | null;
    stream_share_audio: boolean | null;
    stream_audio_bitrate_kbps: number | null;
    stream_enforced_codec: number | null;
  };
}

export async function loadSettings(): Promise<void> {
  let config: LoadedConfigShape;
  try {
    config = await invoke<LoadedConfigShape>("load_config");
  } catch (e) {
    console.warn("[loadSettings] load_config failed; using defaults:", e);
    return;
  }

  const { settings } = config;

  // Privacy
  useDmStore.getState().setFriendsOnlyDms(settings.friends_only_dms);

  // Voice / audio
  if (settings.voice_threshold_db != null) {
    useUiStore.getState().setVoiceThresholdDb(settings.voice_threshold_db);
  }
  useUiStore.getState().setStreamStereo(settings.stream_stereo);
  useUiStore.getState().setInputDevice(settings.input_device);
  useUiStore.getState().setOutputDevice(settings.output_device);
  if (settings.separate_stream_output) {
    useUiStore.getState().setSeparateStreamOutput(true);
    useUiStore.getState().setStreamOutputDevice(settings.stream_output_device);
  }
  useUiStore.getState().setAecEnabled(settings.aec_enabled);
  useUiStore.getState().setNoiseSuppressionLevel(settings.noise_suppression_level);
  useUiStore.getState().setAgcEnabled(settings.agc_enabled);

  // Network
  useUiStore.getState().setUploadLimitBps(settings.upload_limit_bps || 0);
  useUiStore.getState().setDownloadLimitBps(settings.download_limit_bps || 0);
  invoke("set_transfer_limits", {
    uploadBps: settings.upload_limit_bps || 0,
    downloadBps: settings.download_limit_bps || 0,
  }).catch((e) => console.warn("[loadSettings] set_transfer_limits failed:", e));

  // 0 means "no value persisted" — keep the in-store default of 10.
  useUiStore.getState().setChannelCacheSize(settings.channel_cache_size || 10);

  // Media-player volumes. null = never saved → keep store defaults
  // instead of overwriting them with 0.
  if (settings.media_audio_volume != null) {
    useUiStore.getState().setMediaAudioVolume(settings.media_audio_volume);
  }
  useUiStore.getState().setMediaAudioMuted(settings.media_audio_muted);
  if (settings.media_video_volume != null) {
    useUiStore.getState().setMediaVideoVolume(settings.media_video_volume);
  }
  useUiStore.getState().setMediaVideoMuted(settings.media_video_muted);

  // Stream / screen-share. Each field whitelist-validated so a
  // corrupt config can't push the UI into an invalid state. The
  // enforcedCodec gets re-validated once codec caps probe completes,
  // because hardware support can change between launches.
  const validResolutions = ["1080p", "720p", "source"] as const;
  const validFps = [120, 60, 30, 15] as const;
  const validQuality = ["high", "medium", "low", "custom"] as const;
  const validAudioBitrate = [128, 192] as const;
  const restored: Partial<{
    resolution: (typeof validResolutions)[number];
    fps: (typeof validFps)[number];
    quality: (typeof validQuality)[number];
    videoBitrateKbps: number;
    shareAudio: boolean;
    audioBitrateKbps: (typeof validAudioBitrate)[number];
    enforcedCodec: VideoCodec;
  }> = {};
  if (
    settings.stream_resolution &&
    (validResolutions as readonly string[]).includes(settings.stream_resolution)
  ) {
    restored.resolution = settings.stream_resolution as (typeof validResolutions)[number];
  }
  if (
    settings.stream_fps != null &&
    (validFps as readonly number[]).includes(settings.stream_fps)
  ) {
    restored.fps = settings.stream_fps as (typeof validFps)[number];
  }
  if (
    settings.stream_quality &&
    (validQuality as readonly string[]).includes(settings.stream_quality)
  ) {
    restored.quality = settings.stream_quality as (typeof validQuality)[number];
  }
  if (
    settings.stream_video_bitrate_kbps != null &&
    settings.stream_video_bitrate_kbps > 0 &&
    settings.stream_video_bitrate_kbps < 200000
  ) {
    restored.videoBitrateKbps = settings.stream_video_bitrate_kbps;
  }
  if (settings.stream_share_audio != null) {
    restored.shareAudio = settings.stream_share_audio;
  }
  if (
    settings.stream_audio_bitrate_kbps != null &&
    (validAudioBitrate as readonly number[]).includes(settings.stream_audio_bitrate_kbps)
  ) {
    restored.audioBitrateKbps = settings.stream_audio_bitrate_kbps as (typeof validAudioBitrate)[number];
  }
  if (
    settings.stream_enforced_codec != null &&
    [0, 1, 2, 3, 4].includes(settings.stream_enforced_codec)
  ) {
    restored.enforcedCodec = settings.stream_enforced_codec as VideoCodec;
  }
  if (Object.keys(restored).length > 0) {
    // Apply directly via setState so we don't trip the "save settings
    // on every change" path the UI uses; we just loaded these.
    useVoiceStore.setState((state) => ({
      streamSettings: { ...state.streamSettings, ...restored },
    }));
  }

  // Per-user volume + local mute restoration.
  if (settings.user_volumes) {
    for (const [username, db] of Object.entries(settings.user_volumes)) {
      useVoiceStore.getState().setUserVolume(username, db);
    }
  }
  if (settings.local_muted_users) {
    for (const username of settings.local_muted_users) {
      useVoiceStore.getState().toggleLocalMute(username);
    }
  }

  // Auto-login. If the user has saved credentials we attempt them
  // silently; failure (wrong password / server gone / no internet)
  // just lands them on the login screen with no error toast.
  if (config.credentials) {
    useAuthStore.getState().setLoggingIn(true);
    try {
      await invoke("login", {
        username: config.credentials.username,
        password: config.credentials.password,
      });
    } catch {
      useAuthStore.getState().setLoginError(null);
    }
  }
}
