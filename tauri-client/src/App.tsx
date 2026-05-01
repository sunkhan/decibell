import { useState, useEffect, Component, type ReactNode } from "react";
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import AppLayout from "./layouts/AppLayout";
import MainLayout from "./layouts/MainLayout";
import LoginPage from "./features/auth/LoginPage";
import { useAuthStore } from "./stores/authStore";
import { useUiStore } from "./stores/uiStore";
import { useDmStore } from "./stores/dmStore";
import { useVoiceStore } from "./stores/voiceStore";
import { useCodecSettingsStore } from "./stores/codecSettingsStore";
import { VideoCodec } from "./types";
import { useWindowTitle } from "./hooks/useWindowTitle";
import { useDragDrop } from "./features/chat/useDragDrop";
import { usePasteToAttach } from "./features/chat/usePasteToAttach";

class ErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-screen w-screen flex-col items-center justify-center gap-4 bg-bg-primary p-8 text-text-primary">
          <h1 className="text-xl font-bold text-error">Something went wrong</h1>
          <pre className="max-w-[600px] overflow-auto rounded-xl bg-bg-secondary p-4 text-sm text-text-muted">
            {this.state.error.message}
            {"\n"}
            {this.state.error.stack}
          </pre>
          <button
            onClick={() => {
              this.setState({ error: null });
              window.location.reload();
            }}
            className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover"
          >
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

function ProtectedRoute({ children }: { children: ReactNode }) {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }
  return <>{children}</>;
}

export default function App() {
  const [ready, setReady] = useState(false);
  useWindowTitle();
  useDragDrop();
  usePasteToAttach();

  useEffect(() => {
    (async () => {
      try {
        const config = await invoke<{
          credentials?: { username: string; password: string };
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
            stream_resolution: string | null;
            stream_fps: number | null;
            stream_quality: string | null;
            stream_video_bitrate_kbps: number | null;
            stream_share_audio: boolean | null;
            stream_audio_bitrate_kbps: number | null;
            stream_enforced_codec: number | null;
          };
        }>("load_config");

        // Apply saved settings to stores
        const { settings } = config;
        useDmStore.getState().setFriendsOnlyDms(settings.friends_only_dms);
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

        // Attachment transfer caps. Persist in uiStore for the settings UI
        // and also push them into the Rust AppState so in-flight transfers
        // honor them immediately.
        useUiStore.getState().setUploadLimitBps(settings.upload_limit_bps || 0);
        useUiStore.getState().setDownloadLimitBps(settings.download_limit_bps || 0);
        invoke("set_transfer_limits", {
          uploadBps: settings.upload_limit_bps || 0,
          downloadBps: settings.download_limit_bps || 0,
        }).catch(console.error);

        // 0 means "no value persisted" — use the in-store default of 10.
        useUiStore.getState().setChannelCacheSize(settings.channel_cache_size || 10);

        // Restore media-player volumes. null = never saved → keep store
        // defaults (1.0 / false) instead of overwriting them with 0.
        if (settings.media_audio_volume != null) {
          useUiStore.getState().setMediaAudioVolume(settings.media_audio_volume);
        }
        useUiStore.getState().setMediaAudioMuted(settings.media_audio_muted);
        if (settings.media_video_volume != null) {
          useUiStore.getState().setMediaVideoVolume(settings.media_video_volume);
        }
        useUiStore.getState().setMediaVideoMuted(settings.media_video_muted);

        // Restore stream/screen-share settings. Each field is whitelist-
        // validated so a corrupt or stale config can't push the UI into
        // an invalid state — invalid values silently fall back to the
        // store defaults. enforcedCodec is re-validated below once the
        // codec caps probe completes (encoder support can change between
        // launches).
        const validResolutions = ["1080p", "720p", "source"] as const;
        const validFps = [120, 60, 30, 15] as const;
        const validQuality = ["high", "medium", "low", "custom"] as const;
        const validAudioBitrate = [128, 192] as const;
        const restored: Partial<{
          resolution: typeof validResolutions[number];
          fps: typeof validFps[number];
          quality: typeof validQuality[number];
          videoBitrateKbps: number;
          shareAudio: boolean;
          audioBitrateKbps: typeof validAudioBitrate[number];
          enforcedCodec: VideoCodec;
        }> = {};
        if (settings.stream_resolution && (validResolutions as readonly string[]).includes(settings.stream_resolution)) {
          restored.resolution = settings.stream_resolution as typeof validResolutions[number];
        }
        if (settings.stream_fps != null && (validFps as readonly number[]).includes(settings.stream_fps)) {
          restored.fps = settings.stream_fps as typeof validFps[number];
        }
        if (settings.stream_quality && (validQuality as readonly string[]).includes(settings.stream_quality)) {
          restored.quality = settings.stream_quality as typeof validQuality[number];
        }
        if (settings.stream_video_bitrate_kbps != null && settings.stream_video_bitrate_kbps > 0 && settings.stream_video_bitrate_kbps < 200000) {
          restored.videoBitrateKbps = settings.stream_video_bitrate_kbps;
        }
        if (settings.stream_share_audio != null) {
          restored.shareAudio = settings.stream_share_audio;
        }
        if (settings.stream_audio_bitrate_kbps != null && (validAudioBitrate as readonly number[]).includes(settings.stream_audio_bitrate_kbps)) {
          restored.audioBitrateKbps = settings.stream_audio_bitrate_kbps as typeof validAudioBitrate[number];
        }
        if (settings.stream_enforced_codec != null && [0, 1, 2, 3, 4].includes(settings.stream_enforced_codec)) {
          restored.enforcedCodec = settings.stream_enforced_codec as VideoCodec;
        }
        if (Object.keys(restored).length > 0) {
          // Apply directly via set() to avoid the auto-persist round-trip
          // (we just loaded these values). The next user-initiated change
          // will save through setStreamSettings normally.
          useVoiceStore.setState((state) => ({
            streamSettings: { ...state.streamSettings, ...restored },
          }));
        }

        // Restore per-user volume and mute settings
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

        // Auto-login if credentials saved
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
      } catch {
        // No config file or load failed
      }
      // Probe codec capabilities at startup. The watch button gates on
      // decodeCaps via canWatchStream; if we don't load the caps eagerly,
      // a fresh launch shows every stream as unwatchable until the user
      // opens Settings → Codecs (which is the only other place that
      // triggers `load`). Run after the config load so we don't compete
      // for IPC time during the auth path. Doesn't block readiness — if
      // it fails, codec gating just stays conservative until the user
      // opens the Codecs tab.
      useCodecSettingsStore.getState().load().then(() => {
        // Re-validate the saved enforcedCodec against the freshly
        // probed encodeCaps. If the user's hardware lost support for
        // the codec they last forced (driver swap, hardware change),
        // downgrade to UNKNOWN/Auto and persist so the streamer doesn't
        // try to encode with a codec it can't open.
        const enforced = useVoiceStore.getState().streamSettings.enforcedCodec;
        if (enforced !== VideoCodec.UNKNOWN) {
          const encodeCaps = useCodecSettingsStore.getState().encodeCaps;
          const supported = encodeCaps.some((c) => c.codec === enforced);
          if (!supported) {
            console.warn(`[startup] saved enforcedCodec ${enforced} no longer encodable; falling back to Auto`);
            useVoiceStore.getState().setStreamSettings({ enforcedCodec: VideoCodec.UNKNOWN });
          }
        }
      }).catch((e) =>
        console.error("[startup] codec caps load failed:", e)
      );
      setReady(true);
    })();
  }, []);

  if (!ready) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-bg-primary">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-accent border-t-transparent" />
      </div>
    );
  }

  return (
    <ErrorBoundary>
      <BrowserRouter>
        <Routes>
          <Route element={<AppLayout />}>
            <Route path="/login" element={<LoginPage />} />
            <Route path="/" element={<ProtectedRoute><MainLayout /></ProtectedRoute>} />
          </Route>
        </Routes>
      </BrowserRouter>
    </ErrorBoundary>
  );
}
