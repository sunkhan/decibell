import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import MainLayout from "./layouts/MainLayout";
import LoginPage from "./features/auth/LoginPage";
import SettingsPage from "./pages/SettingsPage";
import { useAuthStore } from "./stores/authStore";

function ProtectedRoute({ children }: { children: React.ReactNode }) {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }
  return <>{children}</>;
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<AppLayout />}>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/" element={<ProtectedRoute><MainLayout /></ProtectedRoute>} />
          <Route path="/settings" element={<ProtectedRoute><SettingsPage /></ProtectedRoute>} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
