import { useAuthStore } from "../../stores/authStore";
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
} as const;

/// The permission bits surfaced in role editors, in display order.
/// The reserved default-on bits (send/connect/stream) are hidden until
/// the server actually enforces them.
export const EDITABLE_PERMISSIONS: Array<{
  bit: number;
  label: string;
  description: string;
}> = [
  {
    bit: PERM.ADMINISTRATOR,
    label: "Administrator",
    description: "Grants every permission. Hierarchy still applies.",
  },
  {
    bit: PERM.MANAGE_SERVER,
    label: "Manage Server",
    description: "Change the server name, description and picture.",
  },
  {
    bit: PERM.MANAGE_CHANNELS,
    label: "Manage Channels",
    description: "Edit channel retention settings and wipe history.",
  },
  {
    bit: PERM.MANAGE_ROLES,
    label: "Manage Roles",
    description: "Create, edit and assign roles below their own.",
  },
  {
    bit: PERM.KICK_MEMBERS,
    label: "Kick Members",
    description: "Remove lower-ranked members from the server.",
  },
  {
    bit: PERM.BAN_MEMBERS,
    label: "Ban Members",
    description: "Ban/unban lower-ranked members and see the ban list.",
  },
  {
    bit: PERM.MANAGE_MESSAGES,
    label: "Manage Messages",
    description: "Delete other members' messages.",
  },
  {
    bit: PERM.MANAGE_INVITES,
    label: "Manage Invites",
    description: "Create, list and revoke invites.",
  },
  {
    bit: PERM.MANAGE_NICKNAMES,
    label: "Manage Nicknames",
    description: "Change lower-ranked members' nicknames.",
  },
];

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
  const members = useChatStore((s) =>
    serverId ? s.membersByServer[serverId] : undefined,
  );
  if (!serverId || !localUsername) return false;
  if (!!owner && owner === localUsername) return true;
  const me = members?.find((m) => m.username === localUsername);
  const perms = computeEffectivePermissions(roles, me?.roleIds);
  return hasBits(perms, perm);
}

/// Hierarchy context for moderation UI: the local user's level, their
/// ownership flag, and a resolver for any member's level. Buttons that
/// act on another member should only show when
/// `isOwner || levelOf(target) < level` — mirroring the server's gate.
export function useHierarchy(serverId: string | null): {
  isOwner: boolean;
  level: number;
  levelOf: (username: string) => number;
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
  return { isOwner, level, levelOf };
}
