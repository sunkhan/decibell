export interface User {
  username: string;
  status: "online" | "offline";
}

export interface FriendInfo {
  username: string;
  status: "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked";
}

export interface CommunityServer {
  id: string;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

export interface Channel {
  id: string;
  name: string;
  type: "text" | "voice";
  voiceBitrateKbps?: number;
  retentionDaysText: number;     // 0 = forever
  retentionDaysImage: number;
  retentionDaysVideo: number;
  retentionDaysDocument: number;
  retentionDaysAudio: number;
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
  purgedAt: number; // 0 = present; nonzero unix seconds = tombstone
  // Intrinsic pixel dimensions, 0 when unknown (non-image or legacy row).
  // Used client-side to reserve placeholder space so images don't cause
  // layout shift when the data URL loads in.
  width: number;
  height: number;
  // Total bytes across all server-stored thumbnail sizes (0 = no
  // thumbnails). Used as a "fetch makes sense" flag.
  thumbnailSizeBytes: number;
  // Bitmask of pre-generated thumbnail sizes available on the server.
  // bit 0 = 320 px long-edge, bit 1 = 640 px, bit 2 = 1280 px. 0 with
  // thumbnailSizeBytes > 0 = legacy single-size upload (320, served
  // from the legacy `.thumb.jpg` path without &size= on the request).
  thumbnailSizesMask: number;
  // Duration in milliseconds for audio + video attachments. 0 = unknown
  // (legacy row, non-media kind, or extraction failed at upload time).
  durationMs: number;
}

export interface Message {
  id: number;        // 0 for DMs, server-assigned for channel messages
  sender: string;
  content: string;
  timestamp: string;
  channelId: string;
  attachments: Attachment[];
}

export interface DmMessage {
  sender: string;
  content: string;
  timestamp: string;
}

export interface VoiceParticipant {
  username: string;
  isMuted: boolean;
  isDeafened: boolean;
  isSpeaking: boolean;
  audioLevel: number;
}

export interface StreamInfo {
  streamId: string;
  ownerUsername: string;
  hasAudio: boolean;
  resolutionWidth: number;
  resolutionHeight: number;
  fps: number;
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
  maxUses: number;   // 0 = unlimited
  uses: number;
}
