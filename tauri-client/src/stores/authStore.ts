import { create } from "zustand";

interface AuthState {
  username: string | null;
  token: string | null;
  isAuthenticated: boolean;
  login: (username: string, token: string) => void;
  logout: () => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  username: null,
  token: null,
  isAuthenticated: false,
  login: (username, token) =>
    set({ username, token, isAuthenticated: true }),
  logout: () =>
    set({ username: null, token: null, isAuthenticated: false }),
}));
