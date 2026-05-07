import { create } from "zustand";

export interface AuthErrorNotice {
  serverId: string;
  message: string;
  errorCode: string;
}

export interface MembershipRevocationNotice {
  serverId: string;
  action: string;
  reason: string;
  actor: string;
}

interface UiState {
  activeModal: string | null;
  connectionStatus: "connected" | "reconnecting" | "disconnected";
  activeView: "home" | "server" | "browse" | "voice" | "dm";
  membersPanelVisible: boolean;
  dmFriendsPanelVisible: boolean;
  authError: AuthErrorNotice | null;
  setAuthError: (err: AuthErrorNotice | null) => void;
  membershipRevocationNotice: MembershipRevocationNotice | null;
  setMembershipRevocationNotice: (notice: MembershipRevocationNotice | null) => void;
  profilePopupUser: string | null;
  profilePopupAnchor: { x: number; y: number } | null;
  profilePopupServerId: string | null;
  contextMenuUser: string | null;
  contextMenuAnchor: { x: number; y: number } | null;
  voiceThresholdDb: number;
  setVoiceThresholdDb: (value: number) => void;
  aecEnabled: boolean;
  setAecEnabled: (value: boolean) => void;
  noiseSuppressionLevel: number;
  setNoiseSuppressionLevel: (value: number) => void;
  agcEnabled: boolean;
  setAgcEnabled: (value: boolean) => void;
  streamStereo: boolean;
  setStreamStereo: (value: boolean) => void;
  uploadLimitBps: number;
  downloadLimitBps: number;
  setUploadLimitBps: (value: number) => void;
  setDownloadLimitBps: (value: number) => void;
  channelCacheSize: number;
  setChannelCacheSize: (value: number) => void;
  mediaAudioVolume: number;
  mediaAudioMuted: boolean;
  mediaVideoVolume: number;
  mediaVideoMuted: boolean;
  setMediaAudioVolume: (value: number) => void;
  setMediaAudioMuted: (value: boolean) => void;
  setMediaVideoVolume: (value: number) => void;
  setMediaVideoMuted: (value: boolean) => void;
  dragActive: boolean;
  dragHoveredKey: string | null;
  setDragActive: (value: boolean) => void;
  setDragHoveredKey: (key: string | null) => void;
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
  toggleMembersPanel: () => void;
  toggleDmFriendsPanel: () => void;
  openModal: (modalId: string) => void;
  closeModal: () => void;
  setConnectionStatus: (status: "connected" | "reconnecting" | "disconnected") => void;
  setActiveView: (view: "home" | "server" | "browse" | "voice" | "dm") => void;
  openProfilePopup: (username: string, anchor: { x: number; y: number }, serverId?: string | null) => void;
  closeProfilePopup: () => void;
  openContextMenu: (username: string, anchor: { x: number; y: number }) => void;
  closeContextMenu: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  activeModal: null,
  connectionStatus: "connected",
  activeView: "home",
  membersPanelVisible: true,
  dmFriendsPanelVisible: true,
  authError: null,
  setAuthError: (err) => set({ authError: err }),
  membershipRevocationNotice: null,
  setMembershipRevocationNotice: (notice) => set({ membershipRevocationNotice: notice }),
  profilePopupUser: null,
  profilePopupAnchor: null,
  profilePopupServerId: null,
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
  uploadLimitBps: 0,
  downloadLimitBps: 0,
  setUploadLimitBps: (value) => set({ uploadLimitBps: value }),
  setDownloadLimitBps: (value) => set({ downloadLimitBps: value }),
  channelCacheSize: 10,
  setChannelCacheSize: (value) => set({ channelCacheSize: value }),
  mediaAudioVolume: 1,
  mediaAudioMuted: false,
  mediaVideoVolume: 1,
  mediaVideoMuted: false,
  setMediaAudioVolume: (value) => set({ mediaAudioVolume: Math.max(0, Math.min(1, value)) }),
  setMediaAudioMuted: (value) => set({ mediaAudioMuted: value }),
  setMediaVideoVolume: (value) => set({ mediaVideoVolume: Math.max(0, Math.min(1, value)) }),
  setMediaVideoMuted: (value) => set({ mediaVideoMuted: value }),
  dragActive: false,
  dragHoveredKey: null,
  setDragActive: (value) => set({ dragActive: value }),
  setDragHoveredKey: (key) => set({ dragHoveredKey: key }),
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
  toggleMembersPanel: () => set((state) => ({ membersPanelVisible: !state.membersPanelVisible })),
  toggleDmFriendsPanel: () => set((state) => ({ dmFriendsPanelVisible: !state.dmFriendsPanelVisible })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setActiveView: (view) => set({ activeView: view }),
  openProfilePopup: (username, anchor, serverId = null) =>
    set({ profilePopupUser: username, profilePopupAnchor: anchor, profilePopupServerId: serverId }),
  closeProfilePopup: () =>
    set({ profilePopupUser: null, profilePopupAnchor: null, profilePopupServerId: null }),
  openContextMenu: (username, anchor) =>
    set({ contextMenuUser: username, contextMenuAnchor: anchor }),
  closeContextMenu: () =>
    set({ contextMenuUser: null, contextMenuAnchor: null }),
}));
