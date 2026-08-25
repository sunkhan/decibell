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
  /// Unix seconds of the last edit; 0/absent = never edited. Drives "(edited)".
  editedAt?: number;
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
  /// Unix seconds until which the member is timed out; 0 = none.
  timedOutUntil?: number;
  /// Ids of the member's assigned roles (the default `everyone` role is
  /// implicit and never listed). Resolve against the server's role list
  /// for names/colors/permissions. Empty on legacy servers.
  roleIds: number[];
}

/// A community-server role, mirrored from RoleInfo on the wire. Roles
/// arrive most-senior-first (position DESC) with `everyone` last.
export interface ServerRole {
  id: number;
  name: string;
  /// 0xRRGGBB display color; 0 = default (no color).
  color: number;
  /// Dense hierarchy position; higher = more senior; 0 = `everyone`.
  position: number;
  /// Bitfield of PERM values — see features/servers/permissions.ts.
  permissions: number;
  /// The seeded `everyone` role: undeletable, position 0, implicit on
  /// every member; only its permissions are editable.
  isDefault: boolean;
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
  /// Moderator-applied (server mute/deafen), persisted on the member.
  isServerMuted?: boolean;
  isServerDeafened?: boolean;
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
  /// Number of watchers currently subscribed to this stream (server-derived).
  watcherCount: number;
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
  /// base64 ThumbHash — ~25 bytes decoding to a blurred preview of the
  /// image. Painted under the <img> so a row that scrolls in before its
  /// bytes arrive shows the picture's colours rather than an empty box.
  /// "" for audio/documents, and for uploads that predate the field.
  placeholder: string;
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
  /// Unix seconds of the last edit; 0/absent = never edited. Drives "(edited)".
  editedAt?: number;
}

export interface ChannelInfo {
  id: string;
  name: string;
  /// "category" rows are grouping headers living in the same ordered
  /// list (Discord's flat model): a channel belongs to the nearest
  /// category above it; channels before the first category are
  /// uncategorized.
  type: "text" | "voice" | "category" | "unknown";
  voiceBitrateKbps: number;
  retentionDaysText: number;
  retentionDaysImage: number;
  retentionDaysVideo: number;
  retentionDaysDocument: number;
  retentionDaysAudio: number;
  /// The local user's resolved permissions in this channel (permissions
  /// v2: server-wide role bits with the channel's overwrites applied;
  /// owner/ADMINISTRATOR = everything). Channel lists arrive
  /// per-recipient and only contain channels the user can VIEW. Gate
  /// composer / attach / voice-join / history UI from this — the server
  /// is authoritative. 0 / undefined on legacy servers (→ no gating).
  myPermissions?: number;
  /// Text channels: seconds between messages per member (0 = off).
  slowmodeSeconds?: number;
}

/// One per-channel permission overwrite (permissions v2).
export interface ChannelOverwrite {
  channelId: string;
  targetType: "role" | "member";
  /// Role id (decimal string) or username.
  targetId: string;
  allow: number;
  deny: number;
}

/// Roster paging state per server (see "Roster protocol" in
/// proto/messages.proto). `revision` is the last roster revision applied;
/// a delta that doesn't continue it means we missed something → refetch.
export interface MemberRosterMeta {
  revision: number;
  totalMembers: number;
  hasMore: boolean;
  nextAfter: string;
  loadingMore: boolean;
}

export interface MemberListReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  members: ServerMember[];
  revision: number;
  totalMembers: number;
  hasMore: boolean;
  nextAfter: string;
  firstPage: boolean;
}

export interface MemberUpsertPayload {
  serverId: string;
  member: ServerMember;
  revision: number;
}

export interface MemberRemovePayload {
  serverId: string;
  username: string;
  revision: number;
}

export interface BanInfo {
  username: string;
  bannedBy: string;
  reason: string;
  bannedAt: number;
  expiresAt: number; // 0 = permanent
}

export interface BanListReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  entries: BanInfo[];
  revision: number;
}

export interface ServerMetaUpdatedPayload {
  serverId: string;
  serverName: string;
  serverDescription: string;
  ownerUsername: string;
  publicListing: boolean;
}

export interface AuditEntry {
  id: number;
  timestamp: number;
  actor: string;
  action: string;
  target: string;
  channelId: string;
  details: string;
}

export interface AuditLogReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  entries: AuditEntry[];
  hasMore: boolean;
}

// Mirrors StorageInfoReceivedPayload in native/src/events.rs (← StorageInfoResponse
// in proto/messages.proto). Byte counts are int64 on the wire; JS numbers hold
// them exactly well past any realistic disk size.
export interface StorageKindUsage {
  kind: number; // Attachment.Kind: 0 image, 1 video, 2 document, 3 audio
  bytes: number;
  count: number;
}
export interface StorageChannelUsage {
  channelId: string;
  bytes: number;
  count: number;
}
export interface StorageLargestItem {
  attachmentId: number;
  filename: string;
  sizeBytes: number;
  channelId: string;
  kind: number;
}
export interface StorageInfoReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  volumeTotalBytes: number;
  volumeAvailableBytes: number;
  attachmentsBytes: number;
  thumbnailsBytes: number;
  databaseBytes: number;
  attachmentCount: number;
  minFreeBytes: number;
  byKind: StorageKindUsage[];
  byChannel: StorageChannelUsage[];
  largest: StorageLargestItem[];
}

export interface VoiceForceNotifyPayload {
  serverId: string;
  action: "moved" | "disconnected";
  channelId: string;
  actor: string;
}

export interface ChannelOverwritesReceivedPayload {
  serverId: string;
  success: boolean;
  message: string;
  channelId: string;
  overwrites: ChannelOverwrite[];
}

export interface CommunityAuthRespondedPayload {
  serverId: string;
  /// Id the connection was opened under; differs from serverId only when
  /// native re-keyed an invite-joined ("host:port") connection onto the
  /// central-assigned id reported by the server.
  requestedServerId: string;
  host: string;
  port: number;
  success: boolean;
  message: string;
  channels: ChannelInfo[];
  errorCode: string;
  serverName: string;
  serverDescription: string;
  ownerUsername: string;
  attachmentPort: number;
  maxAttachmentBytes: number;
  publicListing: boolean;
}

export interface ConnectionEventPayload {
  serverType: "central" | "community";
  serverId: string | null;
}

// ── Wire payloads for chat events ────────────────────────────────

export interface MessageReceivedPayload {
  context: string;
  /// The community server this channel message belongs to; empty for
  /// DMs (context === "dm"). Required to namespace the per-channel
  /// message cache — channel ids alone collide across servers.
  serverId: string;
  sender: string;
  recipient: string;
  content: string;
  timestamp: string;
  id: number;
  attachments: WireAttachment[];
  nonce: string;
  editedAt: number;
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
    editedAt: number;
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
  /// base64 ThumbHash; absent on servers that predate the field.
  placeholder?: string;
}
