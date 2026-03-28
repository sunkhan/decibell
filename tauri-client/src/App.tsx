import { Component, type ReactNode } from "react";
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import MainLayout from "./layouts/MainLayout";
import LoginPage from "./features/auth/LoginPage";
import SettingsPage from "./pages/SettingsPage";
import { useAuthStore } from "./stores/authStore";

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
  return (
    <ErrorBoundary>
      <BrowserRouter>
        <Routes>
          <Route element={<AppLayout />}>
            <Route path="/login" element={<LoginPage />} />
            <Route path="/" element={<ProtectedRoute><MainLayout /></ProtectedRoute>} />
            <Route path="/settings" element={<ProtectedRoute><SettingsPage /></ProtectedRoute>} />
          </Route>
        </Routes>
      </BrowserRouter>
    </ErrorBoundary>
  );
}
