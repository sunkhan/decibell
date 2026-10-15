import { create } from "zustand";

interface AuthState {
  username: string | null;
  /// Central accepts username changes (LoginResponse.renames).
  renames: boolean;
  isAuthenticated: boolean;
  isLoggingIn: boolean;
  loginError: string | null;
  isRegistering: boolean;
  registerResult: { success: boolean; message: string } | null;
  login: (username: string, renames?: boolean) => void;
  logout: () => void;
  setLoggingIn: (v: boolean) => void;
  setLoginError: (msg: string | null) => void;
  setRegistering: (v: boolean) => void;
  setRegisterResult: (r: { success: boolean; message: string } | null) => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  username: null,
  renames: false,
  isAuthenticated: false,
  isLoggingIn: false,
  loginError: null,
  isRegistering: false,
  registerResult: null,
  login: (username, renames = false) =>
    set({ username, renames, isAuthenticated: true, isLoggingIn: false, loginError: null }),
  logout: () =>
    set({
      username: null,
      isAuthenticated: false,
      isLoggingIn: false,
      loginError: null,
      registerResult: null,
    }),
  setLoggingIn: (v) => set({ isLoggingIn: v, loginError: null }),
  setLoginError: (msg) => set({ loginError: msg, isLoggingIn: false }),
  setRegistering: (v) => set({ isRegistering: v, registerResult: null }),
  setRegisterResult: (r) => set({ registerResult: r, isRegistering: false }),
}));
