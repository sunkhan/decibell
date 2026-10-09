import { useAuthStore } from "../../stores/authStore";
import { findMember } from "../../hooks/useDisplayName";
import { useChatStore } from "../../stores/chatStore";
import type { ServerRole } from "../../types";

/// Permission bits — mirror of chatproj.Permission in proto/messages.proto.
/// Wire contract: values never change, only new bits get appended. The
/// field is a uint64 on the wire; JSON numbers keep integers exact up to
/// 2^53, so bits 0..52 are usable from JS. Never touch these masks with
/// the native bitwise operators (see hasBits/toggleBit below).
export const PERM = {
  ADMINISTRATOR: 1,
  MANAGE_SERVER: 2,
  MANAGE_CHANNELS: 4,
  MANAGE_ROLES: 8,
  KICK_MEMBERS: 16,
  BAN_MEMBERS: 32,
  MANAGE_MESSAGES: 64,
  MANAGE_INVITES: 128,
  MANAGE_NICKNAMES: 256,
  SEND_MESSAGES: 1024,
  CONNECT_VOICE: 2048,
  STREAM: 4096,
  // permissions v2 (per-channel overwrites)
  VIEW_CHANNEL: 8192,
  READ_HISTORY: 16384,
  ATTACH_FILES: 32768,
  // server management + moderation
  VIEW_AUDIT_LOG: 65536,
  MODERATE_MEMBERS: 131072,
  // permissions v3 (docs/superpowers/specs/2026-10-09-permissions-v3-design.md)
  MUTE_MEMBERS: 262144, // was VOICE_MODERATE (same bit)
  DEAFEN_MEMBERS: 524288,
  MOVE_MEMBERS: 1048576,
  CREATE_INVITE: 2097152,
  CHANGE_NICKNAME: 4194304,
  EMBED_LINKS: 8388608,
  SPEAK: 16777216,
} as const;

export type PermissionGroup = "General" | "Members" | "Text" | "Voice";

/// The permission bits surfaced in role editors and the per-channel
/// overwrite editor, in display order (grouped). Every bit here is
/// enforced server-side.
export const EDITABLE_PERMISSIONS: Array<{
  bit: number;
  label: string;
  description: string;
  group: PermissionGroup;
}> = [
  {
    bit: PERM.ADMINISTRATOR,
    label: "Administrator",
    description: "Grants every permission. Hierarchy still applies.",
    group: "General",
  },
  {
    bit: PERM.VIEW_CHANNEL,
    label: "View Channel",
    description: "See the channel and receive its messages and presence.",
    group: "General",
  },
  {
    bit: PERM.MANAGE_SERVER,
    label: "Manage Server",
    description: "Change the server name, description, picture and storage settings.",
    group: "General",
  },
  {
    bit: PERM.MANAGE_ROLES,
    label: "Manage Roles",
    description: "Create, edit and assign roles below their own; edit channel permissions.",
    group: "General",
  },
  {
    bit: PERM.MANAGE_CHANNELS,
    label: "Manage Channels",
    description: "Create, edit, reorder and delete channels; retention, slowmode and wipes.",
    group: "General",
  },
  {
    bit: PERM.VIEW_AUDIT_LOG,
    label: "View Audit Log",
    description: "See who did what in server settings.",
    group: "General",
  },
  {
    bit: PERM.CREATE_INVITE,
    label: "Create Invite",
    description: "Invite people, and see or revoke their own invites.",
    group: "Members",
  },
  {
    bit: PERM.MANAGE_INVITES,
    label: "Manage Invites",
    description: "See and revoke everyone's invites.",
    group: "Members",
  },
  {
    bit: PERM.CHANGE_NICKNAME,
    label: "Change Nickname",
    description: "Set their own nickname in this server.",
    group: "Members",
  },
  {
    bit: PERM.MANAGE_NICKNAMES,
    label: "Manage Nicknames",
    description: "Change lower-ranked members' nicknames.",
    group: "Members",
  },
  {
    bit: PERM.KICK_MEMBERS,
    label: "Kick Members",
    description: "Remove lower-ranked members from the server.",
    group: "Members",
  },
  {
    bit: PERM.BAN_MEMBERS,
    label: "Ban Members",
    description: "Ban/unban lower-ranked members and see the ban list.",
    group: "Members",
  },
  {
    bit: PERM.MODERATE_MEMBERS,
    label: "Time Out Members",
    description: "Time out lower-ranked members.",
    group: "Members",
  },
  {
    bit: PERM.READ_HISTORY,
    label: "Read Message History",
    description: "Load messages sent before joining the channel view.",
    group: "Text",
  },
  {
    bit: PERM.SEND_MESSAGES,
    label: "Send Messages",
    description: "Post in text channels.",
    group: "Text",
  },
  {
    bit: PERM.ATTACH_FILES,
    label: "Attach Files",
    description: "Upload files and images with messages.",
    group: "Text",
  },
  {
    bit: PERM.EMBED_LINKS,
    label: "Embed Links",
    description: "Links and GIFs in their messages show previews.",
    group: "Text",
  },
  {
    bit: PERM.MANAGE_MESSAGES,
    label: "Manage Messages",
    description: "Delete other members' messages and ignore slowmode.",
    group: "Text",
  },
  {
    bit: PERM.CONNECT_VOICE,
    label: "Connect",
    description: "Join voice channels.",
    group: "Voice",
  },
  {
    bit: PERM.SPEAK,
    label: "Speak",
    description: "Talk in voice channels. Without it, others don't hear them.",
    group: "Voice",
  },
  {
    bit: PERM.STREAM,
    label: "Stream",
    description: "Share the screen in voice channels.",
    group: "Voice",
  },
  {
    bit: PERM.MUTE_MEMBERS,
    label: "Mute Members",
    description: "Server-mute lower-ranked members in voice.",
    group: "Voice",
  },
  {
    bit: PERM.DEAFEN_MEMBERS,
    label: "Deafen Members",
    description: "Server-deafen lower-ranked members in voice.",
    group: "Voice",
  },
  {
    bit: PERM.MOVE_MEMBERS,
    label: "Move Members",
    description: "Move lower-ranked members to any voice channel they can see, or disconnect them.",
    group: "Voice",
  },
];

export const PERMISSION_GROUPS: PermissionGroup[] = ["General", "Members", "Text", "Voice"];

/// The subset that makes sense as a per-channel overwrite (everything
/// that is channel-scoped on the server).
export const CHANNEL_OVERWRITE_PERMISSIONS = EDITABLE_PERMISSIONS.filter((p) =>
  (
    [
      PERM.VIEW_CHANNEL,
      PERM.READ_HISTORY,
      PERM.SEND_MESSAGES,
      PERM.ATTACH_FILES,
      PERM.EMBED_LINKS,
      PERM.CONNECT_VOICE,
      PERM.SPEAK,
      PERM.STREAM,
      PERM.MUTE_MEMBERS,
      PERM.DEAFEN_MEMBERS,
      PERM.MOVE_MEMBERS,
      PERM.MANAGE_CHANNELS,
      PERM.MANAGE_MESSAGES,
      PERM.MANAGE_ROLES,
    ] as number[]
  ).includes(p.bit),
);

/// "Every permission": all 53 exactly-representable bits. What owners
/// and ADMINISTRATOR holders resolve to.
export const PERM_ALL = Number.MAX_SAFE_INTEGER;

/// True when every bit of `bits` is set in `mask`. BigInt-backed on
/// purpose: JS bitwise operators truncate to 32-bit signed ints, so a
/// plain `(mask & bits) === bits` silently breaks the day a permission
/// bit >= 1<<31 is defined. BigInt keeps all 53 JSON-safe bits exact.
export function hasBits(mask: number, bits: number): boolean {
  return (BigInt(mask) & BigInt(bits)) === BigInt(bits);
}

/// Set or clear one permission bit. Same 32-bit-truncation rationale as
/// hasBits — use this instead of `mask | bit` / `mask & ~bit`.
export function toggleBit(mask: number, bit: number, on: boolean): number {
  const m = BigInt(mask);
  const b = BigInt(bit);
  return Number(on ? m | b : m & ~b);
}

/// OR of `everyone` + the member's assigned roles. ADMINISTRATOR expands
/// to everything. Pure helper — pass store data in.
export function computeEffectivePermissions(
  roles: ServerRole[] | undefined,
  roleIds: number[] | undefined,
): number {
  if (!roles || roles.length === 0) return 0;
  let perms = BigInt(roles.find((r) => r.isDefault)?.permissions ?? 0);
  for (const id of roleIds ?? []) {
    perms |= BigInt(roles.find((r) => r.id === id)?.permissions ?? 0);
  }
  if (perms & BigInt(PERM.ADMINISTRATOR)) return PERM_ALL;
  return Number(perms);
}

/// Hierarchy level = position of the member's highest role (0 with no
/// roles). The owner outranks everything — callers check ownership
/// separately (see useHierarchy).
export function memberLevel(
  roles: ServerRole[] | undefined,
  roleIds: number[] | undefined,
): number {
  if (!roles || !roleIds || roleIds.length === 0) return 0;
  let level = 0;
  for (const id of roleIds) {
    const pos = roles.find((r) => r.id === id)?.position ?? 0;
    if (pos > level) level = pos;
  }
  return level;
}

/// True when the local user holds `perm` (every bit of it) in the given
/// server. The owner always passes. Legacy servers that never sent a
/// role list fall back to owner-only gating, matching their server-side
/// behavior.
export function usePermission(serverId: string | null, perm: number): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  const roles = useChatStore((s) =>
    serverId ? s.rolesByServer[serverId] : undefined,
  );
  // Only my own member record: it keeps its identity across other members'
  // presence deltas, so those no longer re-render every permission-gated
  // component (the channel sidebar, the chat panel).
  const me = useChatStore((s) =>
    serverId && localUsername
      ? findMember(s.membersByServer, serverId, localUsername)
      : undefined,
  );
  if (!serverId || !localUsername) return false;
  if (!!owner && owner === localUsername) return true;
  const perms = computeEffectivePermissions(roles, me?.roleIds);
  return hasBits(perms, perm);
}

/// True when the local user holds `perm` in one specific channel, from
/// the server-resolved `ChannelInfo.myPermissions` (permissions v2). The
/// owner always passes. 0 is a real answer: a category header listed only
/// because a channel under it is visible resolves to nothing.
export function useChannelPermission(
  serverId: string | null,
  channelId: string | null,
  perm: number,
): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  const mine = useChatStore((s) => {
    if (!serverId || !channelId) return undefined;
    return s.channelsByServer[serverId]?.find((c) => c.id === channelId)
      ?.myPermissions;
  });
  if (!serverId || !channelId) return false;
  if (!!owner && owner === localUsername) return true;
  if (mine === undefined) return false;
  return hasBits(mine, perm);
}

/// The role at the member's level — their highest assigned role, or
/// `everyone` with none. Carries the "manage each other" flag.
function highestRole(
  roles: ServerRole[] | undefined,
  roleIds: number[] | undefined,
): ServerRole | undefined {
  const level = memberLevel(roles, roleIds);
  if (level === 0) return roles?.find((r) => r.isDefault);
  return roles?.find((r) => r.position === level && (roleIds ?? []).includes(r.id));
}

/// Hierarchy context for moderation UI: the local user's level, their
/// ownership flag, and a resolver for any member's level. Buttons that
/// act on another member should only show when
/// `isOwner || levelOf(target) < level` — mirroring the server's gate
/// for kick / ban / timeout / roles. Nicknames and voice moderation use
/// `canManage`, which also admits peers (same highest role with
/// "members can manage each other" on — permissions v3).
export function useHierarchy(serverId: string | null): {
  isOwner: boolean;
  level: number;
  levelOf: (username: string) => number;
  canManage: (username: string) => boolean;
} {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  const roles = useChatStore((s) =>
    serverId ? s.rolesByServer[serverId] : undefined,
  );
  const members = useChatStore((s) =>
    serverId ? s.membersByServer[serverId] : undefined,
  );
  const isOwner = !!localUsername && !!owner && owner === localUsername;
  const me = members?.find((m) => m.username === localUsername);
  const level = memberLevel(roles, me?.roleIds);
  const levelOf = (username: string) => {
    const m = members?.find((x) => x.username === username);
    return memberLevel(roles, m?.roleIds);
  };
  const peerFlag = !!highestRole(roles, me?.roleIds)?.manageEachOther;
  const canManage = (username: string) => {
    if (username === owner) return false;
    if (isOwner) return true;
    const theirs = levelOf(username);
    if (theirs < level) return true;
    return peerFlag && theirs === level && username !== localUsername;
  };
  return { isOwner, level, levelOf, canManage };
}
