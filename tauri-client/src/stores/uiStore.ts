import { create } from "zustand";

interface UiState {
  sidebarCollapsed: boolean;
  activeModal: string | null;
  connectionStatus: "connected" | "reconnecting" | "disconnected";
  activeView: "home" | "server" | "browse" | "voice" | "dm";
  membersPanelVisible: boolean;
  dmFriendsPanelVisible: boolean;
  profilePopupUser: string | null;
  profilePopupAnchor: { x: number; y: number } | null;
  contextMenuUser: string | null;
  contextMenuAnchor: { x: number; y: number } | null;
  voiceThresholdDb: number;
  setVoiceThresholdDb: (value: number) => void;
  aecEnabled: boolean;
  setAecEnabled: (value: boolean) => void;
  noiseSuppressionLevel: number; // 0=off, 1=light, 2=moderate, 3=aggressive, 4=very aggressive
  setNoiseSuppressionLevel: (value: number) => void;
  agcEnabled: boolean;
  setAgcEnabled: (value: boolean) => void;
  streamStereo: boolean;
  setStreamStereo: (value: boolean) => void;
  inputDevice: string | null;
  outputDevice: string | null;
  separateStreamOutput: boolean;
  streamOutputDevice: string | null;
  settingsTab: string;
  setInputDevice: (device: string | null) => void;
  setOutputDevice: (device: string | null) => void;
  setSeparateStreamOutput: (value: boolean) => void;
  setStreamOutputDevice: (device: string | null) => void;
  setSettingsTab: (tab: string) => void;
  toggleSidebar: () => void;
  toggleMembersPanel: () => void;
  toggleDmFriendsPanel: () => void;
  openModal: (modalId: string) => void;
  closeModal: () => void;
  setConnectionStatus: (status: "connected" | "reconnecting" | "disconnected") => void;
  setActiveView: (view: "home" | "server" | "browse" | "voice" | "dm") => void;
  openProfilePopup: (username: string, anchor: { x: number; y: number }) => void;
  closeProfilePopup: () => void;
  openContextMenu: (username: string, anchor: { x: number; y: number }) => void;
  closeContextMenu: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  sidebarCollapsed: false, activeModal: null, connectionStatus: "connected", activeView: "home",
  membersPanelVisible: true,
  dmFriendsPanelVisible: true,
  profilePopupUser: null,
  profilePopupAnchor: null,
  contextMenuUser: null,
  contextMenuAnchor: null,
  voiceThresholdDb: -50,
  setVoiceThresholdDb: (value) => set({ voiceThresholdDb: value }),
  aecEnabled: false,
  setAecEnabled: (value) => set({ aecEnabled: value }),
  noiseSuppressionLevel: 0,
  setNoiseSuppressionLevel: (value) => set({ noiseSuppressionLevel: value }),
  agcEnabled: false,
  setAgcEnabled: (value) => set({ agcEnabled: value }),
  streamStereo: false,
  setStreamStereo: (value) => set({ streamStereo: value }),
  inputDevice: null,
  outputDevice: null,
  separateStreamOutput: false,
  streamOutputDevice: null,
  settingsTab: "account",
  setInputDevice: (device) => set({ inputDevice: device }),
  setOutputDevice: (device) => set({ outputDevice: device }),
  setSeparateStreamOutput: (value) => set({ separateStreamOutput: value }),
  setStreamOutputDevice: (device) => set({ streamOutputDevice: device }),
  setSettingsTab: (tab) => set({ settingsTab: tab }),
  toggleSidebar: () => set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),
  toggleMembersPanel: () => set((state) => ({ membersPanelVisible: !state.membersPanelVisible })),
  toggleDmFriendsPanel: () => set((state) => ({ dmFriendsPanelVisible: !state.dmFriendsPanelVisible })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setActiveView: (view) => set({ activeView: view }),
  openProfilePopup: (username, anchor) =>
    set({ profilePopupUser: username, profilePopupAnchor: anchor }),
  closeProfilePopup: () =>
    set({ profilePopupUser: null, profilePopupAnchor: null }),
  openContextMenu: (username, anchor) =>
    set({ contextMenuUser: username, contextMenuAnchor: anchor }),
  closeContextMenu: () =>
    set({ contextMenuUser: null, contextMenuAnchor: null }),
}));
