import type { ReactNode } from "react";
import { HashRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import LoginPage from "./features/auth/LoginPage";
import MainLayout from "./layouts/MainLayout";
import { useAuthEvents } from "./features/auth/useAuthEvents";
import { useChatEvents } from "./features/chat/useChatEvents";
import { useServerEvents } from "./features/servers/useServerEvents";
import { useFriendsEvents } from "./features/friends/useFriendsEvents";
import { useVoiceEvents } from "./features/voice/useVoiceEvents";
import { useAuthStore } from "./stores/authStore";

// HashRouter under both vite dev server (http://) and packaged
// loadFile (file://). file:// has no real navigation history;
// HashRouter sidesteps that without runtime branching.
const Router = HashRouter;

function ProtectedRoute({ children }: { children: ReactNode }) {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  if (!isAuthenticated) return <Navigate to="/login" replace />;
  return <>{children}</>;
}

// All listen()-based hooks live inside the Router so they can call
// useNavigate. They mount once at app start and stay mounted across
// route transitions.
function AppRoutes() {
  useAuthEvents();
  useServerEvents();
  useChatEvents();
  useFriendsEvents();
  useVoiceEvents();
  return (
    <Routes>
      <Route element={<AppLayout />}>
        <Route path="/login" element={<LoginPage />} />
        <Route
          path="/"
          element={
            <ProtectedRoute>
              <MainLayout />
            </ProtectedRoute>
          }
        />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}

export default function App() {
  return (
    <Router>
      <AppRoutes />
    </Router>
  );
}
