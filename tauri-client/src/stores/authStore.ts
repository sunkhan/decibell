import { create } from "zustand";

interface AuthState {
  username: string | null;
  isAuthenticated: boolean;
  isLoggingIn: boolean;
  loginError: string | null;
  isRegistering: boolean;
  registerResult: { success: boolean; message: string } | null;
  login: (username: string) => void;
  logout: () => void;
  setLoggingIn: (v: boolean) => void;
  setLoginError: (msg: string | null) => void;
  setRegistering: (v: boolean) => void;
  setRegisterResult: (result: { success: boolean; message: string } | null) => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  username: null, isAuthenticated: false, isLoggingIn: false, loginError: null, isRegistering: false, registerResult: null,
  login: (username) => set({ username, isAuthenticated: true, isLoggingIn: false, loginError: null }),
  logout: () => set({ username: null, isAuthenticated: false, isLoggingIn: false, loginError: null, registerResult: null }),
  setLoggingIn: (v) => set({ isLoggingIn: v, loginError: null }),
  setLoginError: (msg) => set({ loginError: msg, isLoggingIn: false }),
  setRegistering: (v) => set({ isRegistering: v, registerResult: null }),
  setRegisterResult: (result) => set({ registerResult: result, isRegistering: false }),
}));
