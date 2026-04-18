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
}

export interface Message {
  sender: string;
  content: string;
  timestamp: string;
  channelId: string;
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
