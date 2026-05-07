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
}

export interface DmMessage {
  sender: string;
  content: string;
  timestamp: string;
}

export interface ServerMember {
  username: string;
  joinedAt: number;
  nickname: string;
  isOwner: boolean;
  isOnline: boolean;
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

/// Returned by `list_capture_sources` napi command. The `sourceType`
/// is a string ("screen" | "window") to keep the JS surface a plain
/// JSON object — no enum gymnastics needed in the renderer.
export interface RawCaptureSource {
  id: string;
  name: string;
  sourceType: "screen" | "window";
  width: number;
  height: number;
  thumbnail: string | null;
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
