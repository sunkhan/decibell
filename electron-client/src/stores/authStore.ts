import { create } from "zustand";

interface AuthState {
  username: string | null;
  /// Our account uid: every "is this me?" check compares it.
  uid: number;
  /// Central accepts username changes (LoginResponse.renames).
  renames: boolean;
  /// Central serves the device list and password changes (LoginResponse.devices).
  devices: boolean;
  isAuthenticated: boolean;
  isLoggingIn: boolean;
  loginError: string | null;
  /// Why the sign-in page is showing when it wasn't our own Log out (this
  /// device was signed out elsewhere, or the password changed). Cleared by
  /// the next sign-in attempt.
  loginNotice: string | null;
  isRegistering: boolean;
  registerResult: { success: boolean; message: string } | null;
  login: (username: string, renames?: boolean, uid?: number, devices?: boolean) => void;
  logout: () => void;
  setLoggingIn: (v: boolean) => void;
  setLoginError: (msg: string | null) => void;
  setLoginNotice: (msg: string | null) => void;
  setRegistering: (v: boolean) => void;
  setRegisterResult: (r: { success: boolean; message: string } | null) => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  username: null,
  uid: 0,
  renames: false,
  devices: false,
  isAuthenticated: false,
  isLoggingIn: false,
  loginError: null,
  loginNotice: null,
  isRegistering: false,
  registerResult: null,
  login: (username, renames = false, uid = 0, devices = false) =>
    set({ username, uid, renames, devices, isAuthenticated: true, isLoggingIn: false, loginError: null, loginNotice: null }),
  logout: () =>
    set({
      username: null,
      uid: 0,
      isAuthenticated: false,
      isLoggingIn: false,
      loginError: null,
      registerResult: null,
    }),
  setLoggingIn: (v) => set({ isLoggingIn: v, loginError: null, loginNotice: null }),
  setLoginError: (msg) => set({ loginError: msg, isLoggingIn: false }),
  setLoginNotice: (msg) => set({ loginNotice: msg }),
  setRegistering: (v) => set({ isRegistering: v, registerResult: null }),
  setRegisterResult: (r) => set({ registerResult: r, isRegistering: false }),
}));
