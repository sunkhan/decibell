// Shared type definitions for events emitted from Rust and structures
// the renderer consumes. Each interface mirrors a corresponding Rust
// payload struct in native/src/events.rs.

/// Wire-format payload for server entries from the central server's
/// SERVER_LIST_RECEIVED event. Numeric id matches the protobuf shape.
export interface ServerInfoPayload {
  id: number;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

/// In-renderer representation. Server ids are coerced to strings at the
/// listener boundary so they can be used as React keys, Set members,
/// and Record<string,…> keys without numeric coercion footguns.
export interface CommunityServer {
  id: string;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

/// Backwards-compat alias for the wire payload shape — gives the
/// existing useAuthEvents listener a name without churning every
/// reference. New code should reach for CommunityServer directly.
export type ServerInfo = ServerInfoPayload;

export interface FriendInfo {
  username: string;
  status: "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked";
  /// sha256-hex of this friend's current avatar bytes. '' when no
  /// avatar set. avatarStore consumes this for cache invalidation
  /// when FRIEND_LIST_RES arrives. (See spec
  /// docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md §7.)
  avatarVersion: string;
}

/// One online user's snapshot in PresenceUpdate. Mirrors the native
/// UserPresencePayload shape: username + avatar_version. avatarStore
/// uses this to invalidate its cache for non-friend peers too.
export interface UserPresence {
  username: string;
  avatarVersion: string;
}

export interface DmMessage {
  sender: string;
  content: string;
  timestamp: string;
  /// Server-assigned id from DirectMessage.id. Present on messages
  /// that came via DIRECT_MSG after the persistent-DMs feature
  /// shipped, and on every message in DmHistoryRes. Optional /
  /// 0 means "legacy or pre-persistence; can't be marked read
  /// individually". Used to feed `up_to_id` on DmMarkReadReq.
  id?: number;
}

export interface ServerMember {
  username: string;
  joinedAt: number;
  nickname: string;
  isOwner: boolean;
  isOnline: boolean;
}

export interface ServerInvite {
  code: string;
  createdBy: string;
  createdAt: number;
  expiresAt: number; // 0 = never
  maxUses: number; // 0 = unlimited
  uses: number;
}

/// A `decibell://invite/<host>:<port>/<code>` URL parsed from a
/// command-line argument or open-url event. Stashed on the chat
/// store; DeepLinkJoinModal consumes it on next render.
export interface PendingInvite {
  host: string;
  port: number;
  code: string;
}

// ── Voice + streaming types ──────────────────────────────────────

export interface VoiceParticipant {
  username: string;
  isMuted: boolean;
  isDeafened: boolean;
  isSpeaking: boolean;
  audioLevel: number;
}

/// Mirrors the VideoCodec enum in proto/messages.proto. Numeric
/// values must match the wire — they are also the byte stamped in
/// UdpVideoPacket.codec.
export const VideoCodec = {
  UNKNOWN: 0,
  H264_HW: 1,
  H264_SW: 2,
  H265: 3,
  AV1: 4,
} as const;
export type VideoCodec = (typeof VideoCodec)[keyof typeof VideoCodec];

export interface CodecCapability {
  codec: VideoCodec;
  maxWidth: number;
  maxHeight: number;
  maxFps: number;
  /// Whether `hardwareAcceleration: "prefer-hardware"` is reported as
  /// supported by Chromium's WebCodecs probe. Renderer-only metadata —
  /// the codec dropdown surfaces this as a (HW)/(SW) tag so the user
  /// knows whether picking the codec will get GPU encode/decode. Not
  /// shipped to the C++ server; other clients see only codec/dims/fps.
  hardware?: boolean;
}

export interface ClientCapabilities {
  encode: CodecCapability[];
  decode: CodecCapability[];
}

export interface StreamInfo {
  streamId: string;
  ownerUsername: string;
  hasAudio: boolean;
  resolutionWidth: number;
  resolutionHeight: number;
  fps: number;
  currentCodec: VideoCodec;
  enforcedCodec: VideoCodec;
}

/// Notify reasons for StreamCodecChangedNotify (Plan C). Numeric
/// values must match the enum in proto/messages.proto.
export const StreamCodecChangeReason = {
  UNKNOWN: 0,
  WATCHER_JOINED_LOW_CAPS: 1,
  LIMITING_WATCHER_LEFT: 2,
  STREAMER_INITIATED: 3,
} as const;
export type StreamCodecChangeReason =
  (typeof StreamCodecChangeReason)[keyof typeof StreamCodecChangeReason];

/// Wire payload for the STREAM_CODEC_CHANGED bus event. Drives the
/// codec-swap toast notification in the renderer.
export interface StreamCodecChangedNotify {
  channelId: string;
  streamerUsername: string;
  newCodec: VideoCodec;
  newWidth: number;
  newHeight: number;
  newFps: number;
  reason: StreamCodecChangeReason;
}

/// One screen or window enumerated by Chromium's desktopCapturer
/// (bridged via `window.decibell.capture.listSources`). Thumbnail and
/// appIcon are PNG data URLs ready to assign to <img> — Chromium
/// decodes them on assignment, no canvas round-trip needed. Used by
/// the custom screen-share picker on platforms without a native
/// Chromium picker (Windows in Electron 33).
export interface CaptureSource {
  id: string;
  name: string;
  displayId: string;
  appIcon: string;
  thumbnail: string;
  kind: "screen" | "window";
}

/// Returned by `get_caps` and `refresh_caps`. Encode = the FFmpeg-probed
/// hardware encoder list; decode = whatever the renderer's WebCodecs
/// probe shipped to native via `set_decoder_caps`.
export interface CapsResponse {
  encode: CodecCapability[];
  decode: CodecCapability[];
}

/// Persisted in config.json under AppSettings. Each field is round-
/// tripped through `get_codec_settings` / `set_codec_settings` so the
/// CaptureSourcePicker can restore the user's last-used preset.
export interface CodecSettings {
  useAv1: boolean;
  useH265: boolean;
  streamResolution?: string | null;
  streamFps?: number | null;
  streamQuality?: string | null;
  streamVideoBitrateKbps?: number | null;
  streamShareAudio?: boolean | null;
  streamAudioBitrateKbps?: number | null;
  /// VideoCodec byte (0=Auto, 1=H264_HW, 2=H264_SW, 3=H265, 4=AV1).
  streamEnforcedCodec?: number | null;
}

export type AttachmentKind = "image" | "video" | "document" | "audio";

export interface Attachment {
  id: number;
  messageId: number;
  kind: AttachmentKind;
  filename: string;
  mime: string;
  sizeBytes: number;
  url: string;
  position: number;
  createdAt: number;
  /// 0 = present, nonzero = tombstone timestamp
  purgedAt: number;
  width: number;
  height: number;
  thumbnailSizeBytes: number;
  thumbnailSizesMask: number;
  durationMs: number;
}

export interface Message {
  /// 0 means optimistic / unsent. Real messages have a server-assigned id.
  id: number;
  channelId: string;
  sender: string;
  content: string;
  /// String rather than number to round-trip Rust's `i64` (timestamps
  /// can exceed 2^53 in theory, even if not in practice — the existing
  /// Tauri client uses string here too for safety).
  timestamp: string;
  attachments: Attachment[];
  /// Client-generated UUID for optimistic-bubble dedup. Set on outgoing
  /// optimistic placeholders; echoed back on the real broadcast.
  nonce?: string;
  /// Optimistic-bubble link to in-flight uploads in attachmentsStore.
  /// Replaced by the canonical `attachments` array once the server
  /// echoes the broadcast back. Only populated on outgoing
  /// (id === 0) bubbles.
  pendingAttachmentIds?: string[];
}

export interface ChannelInfo {
  id: string;
  name: string;
  type: "text" | "voice" | "unknown";
  voiceBitrateKbps: number;
  retentionDaysText: number;
  retentionDaysImage: number;
  retentionDaysVideo: number;
  retentionDaysDocument: number;
  retentionDaysAudio: number;
}

export interface CommunityAuthRespondedPayload {
  serverId: string;
  success: boolean;
  message: string;
  channels: ChannelInfo[];
  errorCode: string;
  serverName: string;
  serverDescription: string;
  ownerUsername: string;
  attachmentPort: number;
  maxAttachmentBytes: number;
}

export interface ConnectionEventPayload {
  serverType: "central" | "community";
  serverId: string | null;
}

// ── Wire payloads for chat events ────────────────────────────────

export interface MessageReceivedPayload {
  context: string;
  sender: string;
  recipient: string;
  content: string;
  timestamp: string;
  id: number;
  attachments: WireAttachment[];
  nonce: string;
}

export interface ChannelHistoryReceivedPayload {
  serverId: string;
  channelId: string;
  messages: Array<{
    id: number;
    sender: string;
    channelId: string;
    content: string;
    timestamp: number;
    attachments: WireAttachment[];
    nonce: string;
  }>;
  hasMore: boolean;
}

export interface ChannelPrunedPayload {
  serverId: string;
  channelId: string;
  deletedMessageIds: number[];
  purgedAttachments: Array<{ attachmentId: number; purgedAt: number }>;
}

export interface ChannelWipedPayload {
  serverId: string;
  channelId: string;
  wipedAt: number;
  wipedBy: string;
}

export interface ChannelUpdatedPayload {
  serverId: string;
  success: boolean;
  message: string;
  channel: ChannelInfo | null;
}

export interface ChannelWipeRespondedPayload {
  serverId: string;
  channelId: string;
  success: boolean;
  message: string;
  deletedMessageCount: number;
  deletedAttachmentCount: number;
}

interface WireAttachment {
  id: number;
  messageId: number;
  kind: string;
  filename: string;
  mime: string;
  sizeBytes: number;
  url: string;
  position: number;
  createdAt: number;
  purgedAt: number;
  width: number;
  height: number;
  thumbnailSizeBytes: number;
  thumbnailSizesMask: number;
  durationMs: number;
}
