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

  useEffect(() => {
    (async () => {
      try {
        const config = await invoke<{
          credentials?: { username: string; password: string };
          settings: {
            friends_only_dms: boolean;
            stream_stereo: boolean;
            input_device: string | null;
            output_device: string | null;
            separate_stream_output: boolean;
            stream_output_device: string | null;
            user_volumes: Record<string, number>;
            local_muted_users: string[];
          };
        }>("load_config");

        // Apply saved settings to stores
        const { settings } = config;
        useDmStore.getState().setFriendsOnlyDms(settings.friends_only_dms);
        useUiStore.getState().setStreamStereo(settings.stream_stereo);
        useUiStore.getState().setInputDevice(settings.input_device);
        useUiStore.getState().setOutputDevice(settings.output_device);
        if (settings.separate_stream_output) {
          useUiStore.getState().setSeparateStreamOutput(true);
          useUiStore.getState().setStreamOutputDevice(settings.stream_output_device);
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
