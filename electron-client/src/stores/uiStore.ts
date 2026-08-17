import { create } from "zustand";

/// The five selectable palettes. `console-split` is not a palette of
/// its own — it pairs `console` chrome with the `console-light`
/// canvas — but it is a first-class choice as far as the UI and the
/// persisted config are concerned.
export type ThemeId =
  | "graphite"
  | "graphite-light"
  | "console"
  | "console-light"
  | "console-split";

export const THEME_IDS: readonly ThemeId[] = [
  "graphite",
  "graphite-light",
  "console",
  "console-light",
  "console-split",
];

export const DEFAULT_THEME: ThemeId = "graphite";

/// localStorage mirror of the persisted theme. The real store of
/// record is the native config blob, but that only arrives after an
/// async IPC round-trip — long after first paint. The pre-mount script
/// in index.html reads this key so a light theme doesn't flash dark on
/// cold start. Keep the key in sync with public/theme-boot.js.
export const THEME_STORAGE_KEY = "decibell.theme";

/// Text-size and list-density multipliers (Settings → Appearance).
/// Both are plain scalars applied to the type scale and the list-row
/// metrics in globals.css, so a change is one style write on <html>
/// rather than a re-render.
/// Text size is an absolute px value for the message body, stepped on
/// a half-pixel grid so the readout never lands on 14.2px. It is the
/// same number in every theme — the rest of each palette's scale moves
/// with it — so `console*` renders its body at 14.5px too, with its
/// own ratios preserved around it, rather than the 13px it would use
/// unscaled. Mirrored by --ui-text-size-default in globals.css.
export const TEXT_SIZE_MIN_PX = 11;
export const TEXT_SIZE_MAX_PX = 17;
export const TEXT_SIZE_STEP_PX = 0.5;
export const DEFAULT_TEXT_SIZE_PX = 14.5;
export const ROW_SCALE_MIN = 0.85;
export const ROW_SCALE_MAX = 1.3;
export const DEFAULT_ROW_SCALE = 1;

export const TEXT_SIZE_STORAGE_KEY = "decibell.textSizePx";
export const ROW_SCALE_STORAGE_KEY = "decibell.rowScale";

/// Rounded to 2dp because the sliders step in fractions and float
/// drift would otherwise write 1.0500000000000003 into the config.
export function clampScale(value: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback;
  return Math.round(Math.min(max, Math.max(min, value)) * 100) / 100;
}

/// 0 (and anything else out of range) falls back to the default —
/// it's what serde writes for a config predating the field.
export function clampTextSizePx(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return DEFAULT_TEXT_SIZE_PX;
  const snapped = Math.round(value / TEXT_SIZE_STEP_PX) * TEXT_SIZE_STEP_PX;
  return Math.min(TEXT_SIZE_MAX_PX, Math.max(TEXT_SIZE_MIN_PX, snapped));
}

export function applyUiScale(textSizePx: number, rowScale: number): void {
  const root = document.documentElement;
  root.style.setProperty("--ui-text-size-n", String(textSizePx));
  root.style.setProperty("--ui-row-scale", String(rowScale));
  try {
    localStorage.setItem(TEXT_SIZE_STORAGE_KEY, String(textSizePx));
    localStorage.setItem(ROW_SCALE_STORAGE_KEY, String(rowScale));
  } catch {
    // Same trade-off as the theme mirror: the setting still applies
    // this session, it just re-flashes the default on cold start.
  }
}

export function applyTheme(theme: ThemeId): void {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Private mode / quota — the theme still applies this session,
    // it just re-flashes the default on the next cold start.
  }
}

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
  /// Which channel the channel-settings modal targets. Set by the
  /// per-row gear icon via openChannelSettings — the modal no longer
  /// requires the channel to be the active one.
  channelSettingsChannelId: string | null;
  openChannelSettings: (channelId: string) => void;
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
  theme: ThemeId;
  setTheme: (theme: ThemeId) => void;
  textSizePx: number;
  rowScale: number;
  setTextSizePx: (value: number) => void;
  setRowScale: (value: number) => void;
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
  crashReportingEnabled: boolean;
  crashReportingConsentShown: boolean;
  crashReportingInstallId: string | null;
  setCrashReportingEnabled: (v: boolean) => void;
  setCrashReportingConsentShown: (v: boolean) => void;
  setCrashReportingInstallId: (v: string | null) => void;
}

export const useUiStore = create<UiState>((set, get) => ({
  activeModal: null,
  channelSettingsChannelId: null,
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
  theme: DEFAULT_THEME,
  // The attribute flip is what actually re-paints the app — every DS
  // token resolves through it — so the store value is really just a
  // mirror kept around for the settings UI and saveSettings.
  setTheme: (theme) => {
    applyTheme(theme);
    set({ theme });
  },
  textSizePx: DEFAULT_TEXT_SIZE_PX,
  rowScale: DEFAULT_ROW_SCALE,
  setTextSizePx: (value) => {
    const textSizePx = clampTextSizePx(value);
    applyUiScale(textSizePx, get().rowScale);
    set({ textSizePx });
  },
  setRowScale: (value) => {
    const rowScale = clampScale(value, ROW_SCALE_MIN, ROW_SCALE_MAX, DEFAULT_ROW_SCALE);
    applyUiScale(get().textSizePx, rowScale);
    set({ rowScale });
  },
  setInputDevice: (device) => set({ inputDevice: device }),
  setOutputDevice: (device) => set({ outputDevice: device }),
  setSeparateStreamOutput: (value) => set({ separateStreamOutput: value }),
  setStreamOutputDevice: (device) => set({ streamOutputDevice: device }),
  setSettingsTab: (tab) => set({ settingsTab: tab }),
  toggleMembersPanel: () => set((state) => ({ membersPanelVisible: !state.membersPanelVisible })),
  toggleDmFriendsPanel: () => set((state) => ({ dmFriendsPanelVisible: !state.dmFriendsPanelVisible })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  openChannelSettings: (channelId) =>
    set({ activeModal: "channel-settings", channelSettingsChannelId: channelId }),
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
  crashReportingEnabled: true,
  crashReportingConsentShown: false,
  crashReportingInstallId: null,
  setCrashReportingEnabled: (v) => set({ crashReportingEnabled: v }),
  setCrashReportingConsentShown: (v) => set({ crashReportingConsentShown: v }),
  setCrashReportingInstallId: (v) => set({ crashReportingInstallId: v }),
}));
