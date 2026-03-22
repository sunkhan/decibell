import { create } from "zustand";

interface UiState {
  sidebarCollapsed: boolean;
  activeModal: string | null;
  connectionStatus: "connected" | "reconnecting" | "disconnected";
  activeView: "home" | "server" | "browse" | "voice" | "dm";
  profilePopupUser: string | null;
  profilePopupAnchor: { x: number; y: number } | null;
  toggleSidebar: () => void;
  openModal: (modalId: string) => void;
  closeModal: () => void;
  setConnectionStatus: (status: "connected" | "reconnecting" | "disconnected") => void;
  setActiveView: (view: "home" | "server" | "browse" | "voice" | "dm") => void;
  openProfilePopup: (username: string, anchor: { x: number; y: number }) => void;
  closeProfilePopup: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  sidebarCollapsed: false, activeModal: null, connectionStatus: "connected", activeView: "home",
  profilePopupUser: null,
  profilePopupAnchor: null,
  toggleSidebar: () => set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setActiveView: (view) => set({ activeView: view }),
  openProfilePopup: (username, anchor) =>
    set({ profilePopupUser: username, profilePopupAnchor: anchor }),
  closeProfilePopup: () =>
    set({ profilePopupUser: null, profilePopupAnchor: null }),
}));
