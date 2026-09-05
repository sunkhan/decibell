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

/// Layout memory — the resizable left sidebar's width and the mini
/// stream player's width + docked corner. Per-install view state, so it
/// lives in localStorage only, not the native config blob: it isn't a
/// choice made in Settings, and it must not roam to a machine with a
/// different window size. Read once at store creation (synchronously,
/// so MainLayout's first paint already has the width); written by the
/// setters. The bounds live here, not in the components, because a
/// stored value is clamped on read too.
export const SIDEBAR_WIDTH_MIN = 180;
export const SIDEBAR_WIDTH_MAX = 480;
export const SIDEBAR_WIDTH_DEFAULT = 240;
export const PIP_WIDTH_MIN = 240;
export const PIP_WIDTH_MAX = 640;
export const PIP_WIDTH_DEFAULT = 320;
/// P2P call stage (top of the DM) — user-resizable by dragging its bottom
/// edge; one height for the voice tiles, another for a focused stream.
export const CALL_STAGE_MIN = 200;
export const CALL_STAGE_MAX = 1200;
export const CALL_STAGE_VOICE_DEFAULT = 300;
export const CALL_STAGE_STREAM_DEFAULT = 470;
export type PipCorner = "top-left" | "top-right" | "bottom-left" | "bottom-right";
const PIP_CORNERS: readonly string[] = ["top-left", "top-right", "bottom-left", "bottom-right"];
const PIP_CORNER_DEFAULT: PipCorner = "bottom-right";

const SIDEBAR_WIDTH_STORAGE_KEY = "decibell.layout.sidebarWidth";
const PIP_WIDTH_STORAGE_KEY = "decibell.layout.pipWidth";
const CALL_STAGE_VOICE_STORAGE_KEY = "decibell.layout.callStageVoiceHeight";
const CALL_STAGE_STREAM_STORAGE_KEY = "decibell.layout.callStageStreamHeight";
const PIP_CORNER_STORAGE_KEY = "decibell.layout.pipCorner";
const MEMBERS_PANEL_STORAGE_KEY = "decibell.layout.membersPanel";
const DM_FRIENDS_PANEL_STORAGE_KEY = "decibell.layout.dmFriendsPanel";

function clampPx(value: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback;
  return Math.round(Math.min(max, Math.max(min, value)));
}

function readStoredPx(key: string, min: number, max: number, fallback: number): number {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : clampPx(parseFloat(raw), min, max, fallback);
  } catch {
    return fallback;
  }
}

function readStoredBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === "1" ? true : raw === "0" ? false : fallback;
  } catch {
    return fallback;
  }
}

function readStoredCorner(): PipCorner {
  try {
    const raw = localStorage.getItem(PIP_CORNER_STORAGE_KEY);
    return raw !== null && PIP_CORNERS.includes(raw) ? (raw as PipCorner) : PIP_CORNER_DEFAULT;
  } catch {
    return PIP_CORNER_DEFAULT;
  }
}

/// Trailing-coalesced per key: setPipWidth fires on every pointermove
/// of a mini-player resize, and the sidebar drag commits once on
/// mouseup — either way one write per gesture is plenty.
const pendingLayoutWrites = new Map<string, number>();
function writeStoredLater(key: string, value: string): void {
  const prev = pendingLayoutWrites.get(key);
  if (prev !== undefined) window.clearTimeout(prev);
  pendingLayoutWrites.set(
    key,
    window.setTimeout(() => {
      pendingLayoutWrites.delete(key);
      try {
        localStorage.setItem(key, value);
      } catch {
        // Private mode / quota — the layout still applies this session.
      }
    }, 200),
  );
}

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

export interface CertMismatchNotice {
  host: string;
  port: number;
  /// sha256-hex of the certificate the server presented.
  fingerprint: string;
  retry?: () => Promise<void> | void;
}

export interface MembershipRevocationNotice {
  serverId: string;
  action: string;
  reason: string;
  actor: string;
  /// Bans: unix seconds when the ban lifts (0 = permanent).
  expiresAt?: number;
}

interface UiState {
  activeModal: string | null;
  /// Which channel the channel-settings modal targets. Set by the
  /// per-row gear icon via openChannelSettings — the modal no longer
  /// requires the channel to be the active one.
  channelSettingsChannelId: string | null;
  openChannelSettings: (channelId: string) => void;
  /// The full profile screen (features/profile/UserProfileModal): who it
  /// shows and the server it was opened from (nickname + roles). Rides
  /// `activeModal === "user-profile"`; opened from the popup's avatar.
  userProfileUser: string | null;
  userProfileServerId: string | null;
  openUserProfile: (username: string, serverId?: string | null) => void;
  connectionStatus: "connected" | "reconnecting" | "disconnected";
  activeView: "home" | "server" | "browse" | "voice" | "dm";
  /// Which corner the floating pop-out stream player snaps to. Remembered
  /// per install (see the layout-memory block above).
  pipCorner: PipCorner;
  setPipCorner: (c: PipCorner) => void;
  /// Width (px) of the floating pop-out stream player. Height is derived 16:9.
  /// Clamped to [PIP_WIDTH_MIN, PIP_WIDTH_MAX]; remembered per install.
  pipWidth: number;
  setPipWidth: (w: number) => void;
  /// Height (px) of the P2P call stage in its compact form — separate
  /// values for the voice tiles and for a focused stream. Clamped to
  /// [CALL_STAGE_MIN, CALL_STAGE_MAX]; remembered per install.
  callStageVoiceHeight: number;
  callStageStreamHeight: number;
  setCallStageHeight: (mode: "voice" | "stream", h: number) => void;
  /// Width (px) of the resizable left sidebar — one value shared by
  /// ServerChannelsSidebar and ConversationSidebar (only one is mounted
  /// at a time). Clamped to [SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX];
  /// remembered per install.
  sidebarWidth: number;
  setSidebarWidth: (w: number) => void;
  /// Right-hand panel toggles (members list on a server, friends list
  /// beside an open DM). Remembered per install with the rest of the
  /// layout memory.
  membersPanelVisible: boolean;
  dmFriendsPanelVisible: boolean;
  authError: AuthErrorNotice | null;
  setAuthError: (err: AuthErrorNotice | null) => void;
  /// A TLS connection was refused because the server's certificate no
  /// longer matches the pinned / central-reported fingerprint (Theme A).
  /// Drives CertMismatchModal; `retry` re-runs the action after re-trust.
  certMismatch: CertMismatchNotice | null;
  setCertMismatch: (n: CertMismatchNotice | null) => void;
  membershipRevocationNotice: MembershipRevocationNotice | null;
  setMembershipRevocationNotice: (notice: MembershipRevocationNotice | null) => void;
  profilePopupUser: string | null;
  profilePopupAnchor: { x: number; y: number } | null;
  profilePopupServerId: string | null;
  contextMenuUser: string | null;
  contextMenuAnchor: { x: number; y: number } | null;
  /// Server the context menu was opened in, so it can resolve the nickname.
  contextMenuServerId: string | null;
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
  /// When switching voice channels while streaming, carry the stream into the
  /// new channel instead of ending it. Default false.
  takeStreamOnChannelSwitch: boolean;
  setTakeStreamOnChannelSwitch: (value: boolean) => void;
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
  openContextMenu: (
    username: string,
    anchor: { x: number; y: number },
    serverId?: string | null,
  ) => void;
  closeContextMenu: () => void;
  crashReportingEnabled: boolean;
  crashReportingConsentShown: boolean;
  crashReportingInstallId: string | null;
  setCrashReportingEnabled: (v: boolean) => void;
  setCrashReportingConsentShown: (v: boolean) => void;
  setCrashReportingInstallId: (v: string | null) => void;
  /// Unfurl links in messages into preview cards. Off = links stay
  /// plain (still clickable) and no site is contacted until clicked.
  linkPreviewsEnabled: boolean;
  setLinkPreviewsEnabled: (v: boolean) => void;
  /// GIF search without the provider's content filter (explicit
  /// content included). Off = the provider's `low` filter.
  gifUnfiltered: boolean;
  setGifUnfiltered: (v: boolean) => void;
}

export const useUiStore = create<UiState>((set, get) => ({
  activeModal: null,
  channelSettingsChannelId: null,
  userProfileUser: null,
  userProfileServerId: null,
  connectionStatus: "connected",
  activeView: "home",
  pipCorner: readStoredCorner(),
  setPipCorner: (c) => {
    writeStoredLater(PIP_CORNER_STORAGE_KEY, c);
    set({ pipCorner: c });
  },
  pipWidth: readStoredPx(PIP_WIDTH_STORAGE_KEY, PIP_WIDTH_MIN, PIP_WIDTH_MAX, PIP_WIDTH_DEFAULT),
  setPipWidth: (w) => {
    const pipWidth = clampPx(w, PIP_WIDTH_MIN, PIP_WIDTH_MAX, PIP_WIDTH_DEFAULT);
    writeStoredLater(PIP_WIDTH_STORAGE_KEY, String(pipWidth));
    set({ pipWidth });
  },
  callStageVoiceHeight: readStoredPx(
    CALL_STAGE_VOICE_STORAGE_KEY,
    CALL_STAGE_MIN,
    CALL_STAGE_MAX,
    CALL_STAGE_VOICE_DEFAULT,
  ),
  callStageStreamHeight: readStoredPx(
    CALL_STAGE_STREAM_STORAGE_KEY,
    CALL_STAGE_MIN,
    CALL_STAGE_MAX,
    CALL_STAGE_STREAM_DEFAULT,
  ),
  setCallStageHeight: (mode, h) => {
    if (mode === "voice") {
      const v = clampPx(h, CALL_STAGE_MIN, CALL_STAGE_MAX, CALL_STAGE_VOICE_DEFAULT);
      writeStoredLater(CALL_STAGE_VOICE_STORAGE_KEY, String(v));
      set({ callStageVoiceHeight: v });
    } else {
      const v = clampPx(h, CALL_STAGE_MIN, CALL_STAGE_MAX, CALL_STAGE_STREAM_DEFAULT);
      writeStoredLater(CALL_STAGE_STREAM_STORAGE_KEY, String(v));
      set({ callStageStreamHeight: v });
    }
  },
  sidebarWidth: readStoredPx(
    SIDEBAR_WIDTH_STORAGE_KEY,
    SIDEBAR_WIDTH_MIN,
    SIDEBAR_WIDTH_MAX,
    SIDEBAR_WIDTH_DEFAULT,
  ),
  setSidebarWidth: (w) => {
    const sidebarWidth = clampPx(w, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_DEFAULT);
    writeStoredLater(SIDEBAR_WIDTH_STORAGE_KEY, String(sidebarWidth));
    set({ sidebarWidth });
  },
  membersPanelVisible: readStoredBool(MEMBERS_PANEL_STORAGE_KEY, true),
  dmFriendsPanelVisible: readStoredBool(DM_FRIENDS_PANEL_STORAGE_KEY, true),
  authError: null,
  setAuthError: (err) => set({ authError: err }),
  certMismatch: null,
  setCertMismatch: (n) => set({ certMismatch: n }),
  membershipRevocationNotice: null,
  setMembershipRevocationNotice: (notice) => set({ membershipRevocationNotice: notice }),
  profilePopupUser: null,
  profilePopupAnchor: null,
  profilePopupServerId: null,
  contextMenuUser: null,
  contextMenuAnchor: null,
  contextMenuServerId: null,
  voiceThresholdDb: -50,
  setVoiceThresholdDb: (value) => set({ voiceThresholdDb: value }),
  aecEnabled: false,
  setAecEnabled: (value) => set({ aecEnabled: value }),
  noiseSuppressionLevel: 0,
  setNoiseSuppressionLevel: (value) => set({ noiseSuppressionLevel: value }),
  agcEnabled: true,
  setAgcEnabled: (value) => set({ agcEnabled: value }),
  streamStereo: false,
  setStreamStereo: (value) => set({ streamStereo: value }),
  takeStreamOnChannelSwitch: false,
  setTakeStreamOnChannelSwitch: (value) => set({ takeStreamOnChannelSwitch: value }),
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
  toggleMembersPanel: () => {
    const membersPanelVisible = !get().membersPanelVisible;
    writeStoredLater(MEMBERS_PANEL_STORAGE_KEY, membersPanelVisible ? "1" : "0");
    set({ membersPanelVisible });
  },
  toggleDmFriendsPanel: () => {
    const dmFriendsPanelVisible = !get().dmFriendsPanelVisible;
    writeStoredLater(DM_FRIENDS_PANEL_STORAGE_KEY, dmFriendsPanelVisible ? "1" : "0");
    set({ dmFriendsPanelVisible });
  },
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  openChannelSettings: (channelId) =>
    set({ activeModal: "channel-settings", channelSettingsChannelId: channelId }),
  openUserProfile: (username, serverId = null) =>
    set({
      activeModal: "user-profile",
      userProfileUser: username,
      userProfileServerId: serverId,
      // The anchored popup is what opened us; it has no place under a modal.
      profilePopupUser: null,
      profilePopupAnchor: null,
      profilePopupServerId: null,
    }),
  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setActiveView: (view) => set({ activeView: view }),
  openProfilePopup: (username, anchor, serverId = null) =>
    set({ profilePopupUser: username, profilePopupAnchor: anchor, profilePopupServerId: serverId }),
  closeProfilePopup: () =>
    set({ profilePopupUser: null, profilePopupAnchor: null, profilePopupServerId: null }),
  openContextMenu: (username, anchor, serverId = null) =>
    set({
      contextMenuUser: username,
      contextMenuAnchor: anchor,
      contextMenuServerId: serverId,
    }),
  closeContextMenu: () =>
    set({
      contextMenuUser: null,
      contextMenuAnchor: null,
      contextMenuServerId: null,
    }),
  crashReportingEnabled: true,
  crashReportingConsentShown: false,
  crashReportingInstallId: null,
  setCrashReportingEnabled: (v) => set({ crashReportingEnabled: v }),
  setCrashReportingConsentShown: (v) => set({ crashReportingConsentShown: v }),
  setCrashReportingInstallId: (v) => set({ crashReportingInstallId: v }),
  linkPreviewsEnabled: true,
  setLinkPreviewsEnabled: (v) => set({ linkPreviewsEnabled: v }),
  gifUnfiltered: false,
  setGifUnfiltered: (v) => set({ gifUnfiltered: v }),
}));
