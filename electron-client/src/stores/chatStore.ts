import { create } from "zustand";
import type {
  BanInfo,
  ChannelInfo,
  ChannelOverwrite,
  CommunityServer,
  MemberRosterMeta,
  Message,
  PendingInvite,
  ServerInvite,
  ServerMember,
  ServerRole,
} from "../types";
import { useUiStore } from "./uiStore";
import { channelKey, type ChannelKey } from "../lib/channelKey";

// PR4 chatStore — text channels, messages, history paging, optimistic
// bubbles. Members/bans/invites are deferred to later PRs. The LRU
// channel cache uses `channelAccessOrder` (most-recent first); each
// `setActiveChannel` moves the channel to the front and prunes any
// tail beyond `useUiStore.channelCacheSize` to keep RAM bounded.
// `enforceChannelCacheSize` runs the same prune on demand (called from
// NetworkTab when the user lowers the cap mid-session).
//
// Every per-channel map is keyed by ChannelKey (serverId + channelId)
// — bare channel ids collide across servers (each has a "general").
// Actions take serverId + channelId explicitly and compose internally.

interface ChatState {
  // Connection state
  /// Servers the user is a MEMBER of — the ServerBar's source. Populated from
  /// LOGIN_RES memberships and COMMUNITY_AUTH backfill (mergeServers); never
  /// from the discovery directory. Keep this separate from
  /// `discoverableServers` so a directory refresh can't add/remove the user's
  /// own tiles (and private servers, which the directory never lists, still
  /// appear in the bar).
  servers: CommunityServer[];
  /// Public discovery-directory results — the Discover/browse view's source.
  /// Populated only by server_list_received; wholesale-replaced each refresh.
  discoverableServers: CommunityServer[];
  onlineUsers: string[];
  activeServerId: string | null;
  activeChannelId: string | null;
  connectedServers: Set<string>;
  /// Auto-rejoin: server IDs the client is auto-connecting to as a
  /// result of LoginResponse.memberships. Drives the "connecting…"
  /// placeholder tile UI in ServerBar. Each entry is cleared in
  /// useServerEvents on the matching community_auth_responded —
  /// success moves it into connectedServers; failure drops it +
  /// fires request_drop_membership + toasts.
  pendingMembershipServerIds: Set<string>;
  /// Per-server sha256-hex picture version. '' = no picture set.
  /// Populated from CommunityServerInfo payloads (server_list_received,
  /// memberships_received) and from server_picture_changed events.
  serverPictureVersions: Record<string, string>;
  /// Per-server cached image as a data URL. Populated lazily by the
  /// fetch effect when a tile sees a non-empty version with no
  /// cached bytes.
  serverPictures: Record<string, string>;
  serverMeta: Record<string, { name: string; description: string }>;
  serverOwner: Record<string, string>;
  serverPublicListing: Record<string, boolean>;
  membersByServer: Record<string, ServerMember[]>;
  bansByServer: Record<string, BanInfo[]>;
  /// Paging / revision state for membersByServer (roster protocol).
  memberRosterMeta: Record<string, MemberRosterMeta>;
  /// Per-server role list, most-senior-first, `everyone` last. Absent /
  /// empty for legacy servers that predate roles — permission hooks
  /// fall back to owner-only gating in that case.
  rolesByServer: Record<string, ServerRole[]>;
  /// Per-channel permission overwrites, keyed by ChannelKey. Only
  /// populated for channels whose overwrites the local user may view
  /// (MANAGE_ROLES / MANAGE_CHANNELS there); refreshed by the server
  /// push after any change.
  overwritesByChannel: Record<string, ChannelOverwrite[]>;
  invitesByServer: Record<string, ServerInvite[]>;
  pendingInvite: PendingInvite | null;
  /// Attachment HTTP endpoint advertised by each connected server.
  /// `port: 0` means the server didn't advertise one (HTTP disabled
  /// or older build). Populated from CommunityAuthResponded.
  serverAttachmentConfig: Record<string, { port: number; maxBytes: number }>;

  // Channel + message state
  channelsByServer: Record<string, ChannelInfo[]>;
  messagesByChannel: Record<ChannelKey, Message[]>;
  /// Per-channel snapshot of messages that have been optimistically
  /// removed but whose server delete-ack hasn't landed yet. On
  /// rejection (channel_message_delete_responded with success=false)
  /// or watchdog timeout, the snapshot is re-inserted via
  /// mergeMessage + a toast is surfaced. Keyed by ChannelKey →
  /// (messageId → Message).
  pendingDeletions: Record<ChannelKey, Map<number, Message>>;
  hasMoreHistory: Record<ChannelKey, boolean>;
  /// Newer messages exist below the loaded window — true only after a
  /// jump-to-message (around) / downward (after) fetch leaves the view NOT at
  /// the live bottom. false = at present (newest is loaded). Drives the
  /// "jump to present" affordance, downward pagination, and the live-append
  /// guard (a message_received while true is dropped, not appended past the gap).
  hasMoreAfter: Record<ChannelKey, boolean>;
  historyLoading: Record<ChannelKey, boolean>;
  historyFetched: Record<ChannelKey, boolean>;
  /// Per-channel saved scroll position so re-entering a still-cached
  /// channel restores the user to roughly where they left off (Discord-
  /// style). `topIndex` is the topmost-visible Virtuoso item index;
  /// `atBottom` is true if the user was scrolled to the latest message
  /// (in which case we restore by scrolling to LAST so new messages
  /// arrived during the absence are visible). Pruned alongside
  /// messagesByChannel via the LRU eviction in setActiveChannel.
  scrollPositionsByChannel: Record<ChannelKey, { topIndex: number; atBottom: boolean }>;
  /// Live dimensions of the chat panel's viewport. Updated by ChatPanel
  /// via a ResizeObserver. Read by AttachmentList's sqrt-based sizing
  /// so image/video previews scale proportionally to the available
  /// viewport — narrow side panels render compact previews, wide
  /// fullscreen layouts render larger ones, without ever uncapped-
  /// linearly hitting "image takes up half the screen".
  chatViewSize: { width: number; height: number } | null;
  /// LRU access order for cached channels — front (index 0) is the
  /// most recently visited, tail is the least. Channels beyond
  /// `useUiStore.channelCacheSize` get evicted from every per-channel
  /// map below on the next setActiveChannel or enforceChannelCacheSize.
  channelAccessOrder: ChannelKey[];

  // Mutators
  setOnlineUsers: (users: string[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: ChannelInfo[]) => void;
  upsertChannel: (serverId: string, channel: ChannelInfo) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
  /// Auto-rejoin: replace the pending-membership set entirely (used
  /// on memberships_received). Setting an empty array clears.
  setPendingMemberships: (ids: string[]) => void;
  removePendingMembership: (id: string) => void;
  /// Set the picture version for a server. If the new version
  /// differs from the cached one, clears serverPictures[serverId] so
  /// the next tile render lazy-fetches fresh bytes. Idempotent.
  setServerPictureVersion: (serverId: string, version: string) => void;
  /// Cache fetched image bytes (data URL) for a server. Guarded:
  /// only writes if the fetch's version still matches the current
  /// serverPictureVersions[serverId] — a stale fetch landing after
  /// a newer version-changed event is dropped silently.
  setServerPictureData: (serverId: string, version: string, dataUrl: string) => void;
  /// De-duplicating union into the MEMBER `servers` list. Used on
  /// memberships_received and COMMUNITY_AUTH backfill.
  mergeServers: (entries: CommunityServer[]) => void;
  /// Replace the discovery-directory results (Discover view source). Safe to
  /// wholesale-replace: member servers live in `servers`, not here.
  setDiscoverableServers: (entries: CommunityServer[]) => void;
  setServerMeta: (
    serverId: string,
    meta: { name: string; description: string },
  ) => void;
  setServerOwner: (serverId: string, owner: string) => void;
  setServerPublicListing: (serverId: string, on: boolean) => void;
  /// Apply one roster page. The first page replaces the list (it carries
  /// every online member); later pages append offline members.
  applyMemberPage: (
    serverId: string,
    members: ServerMember[],
    meta: { revision: number; totalMembers: number; hasMore: boolean; nextAfter: string; firstPage: boolean },
  ) => void;
  /// Live deltas. Return false when the revision doesn't continue the
  /// last applied one (a gap) so the caller can refetch page 1.
  upsertMember: (serverId: string, member: ServerMember, revision: number) => boolean;
  removeMember: (serverId: string, username: string, revision: number) => boolean;
  setMembersLoadingMore: (serverId: string, loading: boolean) => void;
  setBansForServer: (serverId: string, bans: BanInfo[]) => void;
  setRolesForServer: (serverId: string, roles: ServerRole[]) => void;
  setOverwritesForChannel: (
    serverId: string,
    channelId: string,
    overwrites: ChannelOverwrite[],
  ) => void;
  setInvitesForServer: (serverId: string, invites: ServerInvite[]) => void;
  upsertInvite: (serverId: string, invite: ServerInvite) => void;
  removeInvite: (serverId: string, code: string) => void;
  setPendingInvite: (invite: PendingInvite | null) => void;
  setServerAttachmentConfig: (
    serverId: string,
    port: number,
    maxBytes: number,
  ) => void;
  resetForLogout: () => void;
  addMessage: (serverId: string, message: Message) => void;
  prependHistory: (
    serverId: string,
    channelId: string,
    messages: Message[],
    hasMore: boolean,
  ) => void;
  /// Jump-to-message: REPLACE the channel's loaded messages with a context
  /// window (drops optimistics), setting both older/newer "has more" flags.
  setChannelWindow: (
    serverId: string,
    channelId: string,
    messages: Message[],
    hasMoreOlder: boolean,
    hasMoreNewer: boolean,
  ) => void;
  /// Downward pagination: append a newer page, updating hasMoreAfter.
  appendNewer: (
    serverId: string,
    channelId: string,
    messages: Message[],
    hasMoreNewer: boolean,
  ) => void;
  /// Jump-to-present: clear the loaded slice so the caller can reload the
  /// newest page from scratch. Resets hasMoreAfter → false (we'll be at the
  /// live bottom) and historyFetched → false (a fresh fetch is coming).
  resetChannelForJump: (serverId: string, channelId: string) => void;
  setHistoryLoading: (serverId: string, channelId: string, loading: boolean) => void;
  markHistoryFetched: (serverId: string, channelId: string) => void;
  applyChannelPruned: (
    serverId: string,
    channelId: string,
    deletedMessageIds: number[],
  ) => void;
  applyChannelWiped: (serverId: string, channelId: string) => void;
  /// Drops every per-channel cache entry for a channel that no longer
  /// exists (deleted server-side). Unlike applyChannelWiped this removes
  /// the keys entirely, so a future channel reusing the same slug
  /// refetches history instead of inheriting stale state.
  purgeChannelState: (serverId: string, channelId: string) => void;
  /// Per-message delete: remove from a channel's visible message list.
  /// Idempotent. Same handler runs for "my delete succeeded" and
  /// "someone else deleted this message".
  removeMessage: (serverId: string, channelId: string, messageId: number) => void;
  /// Apply an edit broadcast: replace content + set editedAt on the matching
  /// message. No-op if the message isn't in the cache.
  applyEdit: (
    serverId: string,
    channelId: string,
    messageId: number,
    content: string,
    editedAt: number,
  ) => void;
  /// Remove an optimistic (id === 0) message by its nonce. Used when a
  /// send is abandoned (nothing left to send after all uploads failed) or
  /// the send call rejects, so the placeholder bubble doesn't linger
  /// forever unreconciled.
  removeMessageByNonce: (serverId: string, channelId: string, nonce: string) => void;
  /// Snapshot a message into pendingDeletions, then remove it from
  /// the visible list. Returns the snapshot so the caller knows the
  /// optimistic remove actually happened.
  snapshotAndRemove: (
    serverId: string,
    channelId: string,
    messageId: number,
  ) => Message | undefined;
  /// Re-insert a previously-snapshotted message back into the array
  /// (sorted by id via existing mergeMessage). Also clears the
  /// pending entry. No-op if no matching snapshot exists.
  restorePendingDeletion: (serverId: string, channelId: string, messageId: number) => void;
  /// Drop the pending snapshot (called on success-ack or matching
  /// broadcast). No-op if no matching snapshot exists.
  clearPendingDeletion: (serverId: string, channelId: string, messageId: number) => void;
  /// Capture the user's current scroll position for a channel — called
  /// from ChatPanel on Virtuoso range/atBottom events.
  setScrollPosition: (
    serverId: string,
    channelId: string,
    topIndex: number,
    atBottom: boolean,
  ) => void;
  /// Update the live chat viewport dimensions. Called from ChatPanel's
  /// ResizeObserver. Pass `null` on unmount so AttachmentList's sizing
  /// helpers fall back to their fixed defaults.
  setChatViewSize: (size: { width: number; height: number } | null) => void;
  /// Drop cached channels beyond `useUiStore.channelCacheSize`. Called
  /// when the cap shrinks so eviction is immediate, not deferred to
  /// the next channel switch.
  enforceChannelCacheSize: () => void;
}

// Merge a new message into a channel's list, sorted by id ascending
// and deduped by id. id=0 entries (optimistic bubbles) sit at the tail
// since they have no stable cursor; on receipt of a real server
// message echoing the same nonce, the optimistic is reaped.
//
// The wire-message hot path is "incoming.id > every existing real id"
// (i.e. a fresh server message arriving in order). We special-case
// that to a single tail-scan + one allocation: O(M) work in 2 ops
// instead of the 4 ops the general path needs (filter + double slice
// + spread).
function mergeMessage(existing: Message[], incoming: Message): Message[] {
  // Optimistic bubbles always anchor at the tail; no dedupe needed,
  // no insertion point to compute.
  if (incoming.id === 0) {
    const out = existing.slice();
    out.push(incoming);
    return out;
  }

  const len = existing.length;
  if (len === 0) return [incoming];

  // Single tail-scan: locate the last real id (insertion anchor) and
  // any optimistic in the trailing block whose nonce echoes ours.
  // The trailing block is "everything after the last real" — that's
  // where unsent optimistics live.
  let lastRealIdx = -1;
  let nonceMatchIdx = -1;
  for (let i = len - 1; i >= 0; --i) {
    const m = existing[i];
    if (m.id === 0) {
      if (
        nonceMatchIdx === -1 &&
        incoming.nonce &&
        m.nonce === incoming.nonce
      ) {
        nonceMatchIdx = i;
      }
    } else {
      lastRealIdx = i;
      break;
    }
  }

  // Hot path: incoming is strictly newer than every real id, so we
  // can skip the full O(M) dedupe filter — no real-id collision is
  // possible. One slice + at most one trailing-optimistic removal +
  // one insert.
  if (lastRealIdx === -1 || existing[lastRealIdx].id < incoming.id) {
    const out = existing.slice();
    if (nonceMatchIdx !== -1) out.splice(nonceMatchIdx, 1);
    out.splice(lastRealIdx + 1, 0, incoming);
    return out;
  }

  // Slow path: incoming.id is <= some existing real id (history
  // back-fill, out-of-order delivery, or a duplicate). Match the
  // original ordering semantics: insert AFTER the last real with
  // id < incoming.id, ignoring optimistics for position. Fold the
  // dedupe filter and the position scan into the same allocation.
  const filtered: Message[] = [];
  for (let i = 0; i < len; ++i) {
    const m = existing[i];
    if (m.id === incoming.id) continue;
    if (m.id === 0 && incoming.nonce && m.nonce === incoming.nonce) continue;
    filtered.push(m);
  }
  let idx = filtered.length;
  for (let i = filtered.length - 1; i >= 0; --i) {
    if (filtered[i].id !== 0 && filtered[i].id < incoming.id) {
      idx = i + 1;
      break;
    }
    if (i === 0) idx = 0;
  }
  filtered.splice(idx, 0, incoming);
  return filtered;
}

export const useChatStore = create<ChatState>((set, get) => ({
  servers: [],
  discoverableServers: [],
  onlineUsers: [],
  activeServerId: null,
  activeChannelId: null,
  connectedServers: new Set(),
  pendingMembershipServerIds: new Set(),
  serverPictureVersions: {},
  serverPictures: {},
  serverMeta: {},
  serverOwner: {},
  serverPublicListing: {},
  membersByServer: {},
  bansByServer: {},
  memberRosterMeta: {},
  rolesByServer: {},
  overwritesByChannel: {},
  invitesByServer: {},
  pendingInvite: null,
  serverAttachmentConfig: {},
  channelsByServer: {},
  messagesByChannel: {},
  pendingDeletions: {},
  hasMoreHistory: {},
  hasMoreAfter: {},
  historyLoading: {},
  historyFetched: {},
  scrollPositionsByChannel: {},
  chatViewSize: null,
  channelAccessOrder: [],
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) =>
    set((state) => {
      if (!channelId) return { activeChannelId: null };
      // LRU bookkeeping keys on serverId+channelId; callers set the
      // active server before activating one of its channels (ServerBar
      // does, and every event handler checks activeServerId first).
      if (!state.activeServerId) return { activeChannelId: channelId };
      const key = channelKey(state.activeServerId, channelId);
      const cap = Math.max(1, useUiStore.getState().channelCacheSize || 10);
      // Move the activated channel to the front of the access order.
      const reordered = [
        key,
        ...state.channelAccessOrder.filter((k) => k !== key),
      ];
      if (reordered.length <= cap) {
        return { activeChannelId: channelId, channelAccessOrder: reordered };
      }
      // Over the cap — drop the tail and prune every cached slice for
      // channels that fell off.
      const keep = reordered.slice(0, cap);
      const keepSet = new Set<string>(keep);
      const filter = <T,>(rec: Record<ChannelKey, T>): Record<ChannelKey, T> =>
        Object.fromEntries(
          Object.entries(rec).filter(([k]) => keepSet.has(k)),
        ) as Record<ChannelKey, T>;
      return {
        activeChannelId: channelId,
        channelAccessOrder: keep,
        messagesByChannel: filter(state.messagesByChannel),
        hasMoreHistory: filter(state.hasMoreHistory),
        hasMoreAfter: filter(state.hasMoreAfter),
        historyLoading: filter(state.historyLoading),
        historyFetched: filter(state.historyFetched),
        scrollPositionsByChannel: filter(state.scrollPositionsByChannel),
      };
    }),

  setChannelsForServer: (serverId, channels) =>
    set((state) => ({
      channelsByServer: { ...state.channelsByServer, [serverId]: channels },
    })),

  upsertChannel: (serverId, channel) =>
    set((state) => {
      const existing = state.channelsByServer[serverId] ?? [];
      const idx = existing.findIndex((c) => c.id === channel.id);
      const next =
        idx === -1
          ? [...existing, channel]
          : existing.map((c, i) => (i === idx ? channel : c));
      return { channelsByServer: { ...state.channelsByServer, [serverId]: next } };
    }),

  addConnectedServer: (serverId) =>
    set((state) => ({ connectedServers: new Set([...state.connectedServers, serverId]) })),

  removeConnectedServer: (serverId) =>
    set((state) => {
      const next = new Set(state.connectedServers);
      next.delete(serverId);
      return { connectedServers: next };
    }),

  setPendingMemberships: (ids) =>
    set({ pendingMembershipServerIds: new Set(ids) }),
  removePendingMembership: (id) =>
    set((state) => {
      if (!state.pendingMembershipServerIds.has(id)) return {};
      const next = new Set(state.pendingMembershipServerIds);
      next.delete(id);
      return { pendingMembershipServerIds: next };
    }),
  setServerPictureVersion: (serverId, version) =>
    set((state) => {
      const current = state.serverPictureVersions[serverId] ?? "";
      if (current === version) return {};
      const nextVersions = {
        ...state.serverPictureVersions,
        [serverId]: version,
      };
      // Version changed → invalidate cached bytes; next tile render
      // lazy-fetches the fresh data.
      const nextPictures = { ...state.serverPictures };
      delete nextPictures[serverId];
      return {
        serverPictureVersions: nextVersions,
        serverPictures: nextPictures,
      };
    }),
  setServerPictureData: (serverId, version, dataUrl) =>
    set((state) => {
      const current = state.serverPictureVersions[serverId] ?? "";
      // Drop fetches whose version is no longer current — a newer
      // server_picture_changed event invalidated this fetch before
      // it returned.
      if (current !== version) return {};
      return {
        serverPictures: { ...state.serverPictures, [serverId]: dataUrl },
      };
    }),
  mergeServers: (entries) =>
    set((state) => {
      const byId = new Map<string, CommunityServer>();
      for (const s of state.servers) byId.set(s.id, s);
      for (const s of entries) {
        if (!byId.has(s.id)) byId.set(s.id, s);
      }
      return { servers: Array.from(byId.values()) };
    }),

  setDiscoverableServers: (entries) => set({ discoverableServers: entries }),

  setServerMeta: (serverId, meta) =>
    set((state) => ({ serverMeta: { ...state.serverMeta, [serverId]: meta } })),

  setServerOwner: (serverId, owner) =>
    set((state) => ({ serverOwner: { ...state.serverOwner, [serverId]: owner } })),

  setServerPublicListing: (serverId, on) =>
    set((state) => ({
      serverPublicListing: { ...state.serverPublicListing, [serverId]: on },
    })),

  applyMemberPage: (serverId, members, meta) =>
    set((state) => {
      const existing = meta.firstPage ? [] : state.membersByServer[serverId] ?? [];
      const byName = new Map(existing.map((m) => [m.username, m]));
      for (const m of members) byName.set(m.username, m);
      return {
        membersByServer: { ...state.membersByServer, [serverId]: Array.from(byName.values()) },
        memberRosterMeta: {
          ...state.memberRosterMeta,
          [serverId]: {
            revision: meta.revision,
            totalMembers: meta.totalMembers,
            hasMore: meta.hasMore,
            nextAfter: meta.nextAfter,
            loadingMore: false,
          },
        },
      };
    }),

  upsertMember: (serverId, member, revision) => {
    const state = get();
    const meta = state.memberRosterMeta[serverId];
    // No page loaded yet → nothing to patch; the page fetch will carry it.
    if (!meta) return true;
    if (revision !== meta.revision + 1) return false;
    const list = state.membersByServer[serverId] ?? [];
    const idx = list.findIndex((m) => m.username === member.username);
    const next = idx >= 0 ? list.map((m, i) => (i === idx ? member : m)) : [...list, member];
    set({
      membersByServer: { ...state.membersByServer, [serverId]: next },
      memberRosterMeta: {
        ...state.memberRosterMeta,
        [serverId]: {
          ...meta,
          revision,
          totalMembers: idx >= 0 ? meta.totalMembers : meta.totalMembers + 1,
        },
      },
    });
    return true;
  },

  removeMember: (serverId, username, revision) => {
    const state = get();
    const meta = state.memberRosterMeta[serverId];
    if (!meta) return true;
    if (revision !== meta.revision + 1) return false;
    const list = state.membersByServer[serverId] ?? [];
    const next = list.filter((m) => m.username !== username);
    set({
      membersByServer: { ...state.membersByServer, [serverId]: next },
      memberRosterMeta: {
        ...state.memberRosterMeta,
        [serverId]: {
          ...meta,
          revision,
          totalMembers: Math.max(0, meta.totalMembers - (next.length === list.length ? 0 : 1)),
        },
      },
    });
    return true;
  },

  setMembersLoadingMore: (serverId, loading) =>
    set((state) => {
      const meta = state.memberRosterMeta[serverId];
      if (!meta) return {};
      return { memberRosterMeta: { ...state.memberRosterMeta, [serverId]: { ...meta, loadingMore: loading } } };
    }),

  setBansForServer: (serverId, bans) =>
    set((state) => ({
      bansByServer: { ...state.bansByServer, [serverId]: bans },
    })),

  setRolesForServer: (serverId, roles) =>
    set((state) => ({
      rolesByServer: { ...state.rolesByServer, [serverId]: roles },
    })),

  setOverwritesForChannel: (serverId, channelId, overwrites) =>
    set((state) => ({
      overwritesByChannel: {
        ...state.overwritesByChannel,
        [channelKey(serverId, channelId)]: overwrites,
      },
    })),

  setInvitesForServer: (serverId, invites) =>
    set((state) => ({
      invitesByServer: { ...state.invitesByServer, [serverId]: invites },
    })),

  upsertInvite: (serverId, invite) =>
    set((state) => {
      const existing = state.invitesByServer[serverId] ?? [];
      const filtered = existing.filter((i) => i.code !== invite.code);
      return {
        invitesByServer: {
          ...state.invitesByServer,
          [serverId]: [invite, ...filtered],
        },
      };
    }),

  removeInvite: (serverId, code) =>
    set((state) => {
      const existing = state.invitesByServer[serverId] ?? [];
      return {
        invitesByServer: {
          ...state.invitesByServer,
          [serverId]: existing.filter((i) => i.code !== code),
        },
      };
    }),

  setPendingInvite: (invite) => set({ pendingInvite: invite }),

  setServerAttachmentConfig: (serverId, port, maxBytes) =>
    set((state) => ({
      serverAttachmentConfig: {
        ...state.serverAttachmentConfig,
        [serverId]: { port, maxBytes },
      },
    })),

  addMessage: (serverId, message) =>
    set((state) => {
      const key = channelKey(serverId, message.channelId);
      // While viewing a jumped/windowed slice (not at the live bottom), a live
      // message belongs past a hidden gap — dropping it keeps the window
      // contiguous. It'll load when the user returns to present / pages down.
      // Optimistic sends (id=0) never reach here in that state: the composer
      // jumps to present before sending.
      if (state.hasMoreAfter[key] && message.id !== 0) return {};
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [key]: mergeMessage(state.messagesByChannel[key] ?? [], message),
        },
      };
    }),

  prependHistory: (serverId, channelId, messages, hasMore) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const existing = state.messagesByChannel[key] ?? [];
      // History batch may overlap with already-loaded live messages —
      // dedup by real id (id=0 entries are optimistic, not in history).
      const existingIds = new Set(existing.filter((m) => m.id !== 0).map((m) => m.id));
      const fresh = messages.filter((m) => m.id !== 0 && !existingIds.has(m.id));
      const withId = [...fresh, ...existing.filter((m) => m.id !== 0)].sort(
        (a, b) => a.id - b.id,
      );
      const ephemeral = existing.filter((m) => m.id === 0);
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [key]: [...withId, ...ephemeral],
        },
        hasMoreHistory: { ...state.hasMoreHistory, [key]: hasMore },
      };
    }),

  setChannelWindow: (serverId, channelId, messages, hasMoreOlder, hasMoreNewer) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      // Replace wholesale — a jump window is a fresh, possibly-discontinuous
      // slice. Drop optimistics (they belong at the live tail, not here) and
      // sort defensively by id.
      const list = messages.filter((m) => m.id !== 0).sort((a, b) => a.id - b.id);
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: list },
        hasMoreHistory: { ...state.hasMoreHistory, [key]: hasMoreOlder },
        hasMoreAfter: { ...state.hasMoreAfter, [key]: hasMoreNewer },
        historyFetched: { ...state.historyFetched, [key]: true },
      };
    }),

  appendNewer: (serverId, channelId, messages, hasMoreNewer) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const existing = state.messagesByChannel[key] ?? [];
      const existingIds = new Set(existing.filter((m) => m.id !== 0).map((m) => m.id));
      const fresh = messages.filter((m) => m.id !== 0 && !existingIds.has(m.id));
      const withId = [...existing.filter((m) => m.id !== 0), ...fresh].sort(
        (a, b) => a.id - b.id,
      );
      const ephemeral = existing.filter((m) => m.id === 0);
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: [...withId, ...ephemeral] },
        hasMoreAfter: { ...state.hasMoreAfter, [key]: hasMoreNewer },
      };
    }),

  resetChannelForJump: (serverId, channelId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: [] },
        hasMoreHistory: { ...state.hasMoreHistory, [key]: true },
        hasMoreAfter: { ...state.hasMoreAfter, [key]: false },
        historyFetched: { ...state.historyFetched, [key]: false },
      };
    }),

  setHistoryLoading: (serverId, channelId, loading) =>
    set((state) => ({
      historyLoading: {
        ...state.historyLoading,
        [channelKey(serverId, channelId)]: loading,
      },
    })),

  markHistoryFetched: (serverId, channelId) =>
    set((state) => ({
      historyFetched: {
        ...state.historyFetched,
        [channelKey(serverId, channelId)]: true,
      },
    })),

  applyChannelPruned: (serverId, channelId, deletedMessageIds) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const existing = state.messagesByChannel[key] ?? [];
      const deletedSet = new Set(deletedMessageIds);
      const next = existing.filter((m) => m.id === 0 || !deletedSet.has(m.id));
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: next },
      };
    }),

  applyChannelWiped: (serverId, channelId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: [] },
        hasMoreHistory: { ...state.hasMoreHistory, [key]: false },
        hasMoreAfter: { ...state.hasMoreAfter, [key]: false },
        historyFetched: { ...state.historyFetched, [key]: true },
      };
    }),

  purgeChannelState: (serverId, channelId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const drop = <T,>(rec: Record<ChannelKey, T>): Record<ChannelKey, T> => {
        if (!(key in rec)) return rec;
        const next = { ...rec };
        delete next[key];
        return next;
      };
      return {
        messagesByChannel: drop(state.messagesByChannel),
        hasMoreHistory: drop(state.hasMoreHistory),
        hasMoreAfter: drop(state.hasMoreAfter),
        historyLoading: drop(state.historyLoading),
        historyFetched: drop(state.historyFetched),
        scrollPositionsByChannel: drop(state.scrollPositionsByChannel),
      };
    }),

  removeMessage: (serverId, channelId, messageId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const list = state.messagesByChannel[key];
      if (!list) return {};
      const next = list.filter((m) => m.id !== messageId);
      if (next.length === list.length) return {};
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [key]: next,
        },
      };
    }),

  applyEdit: (serverId, channelId, messageId, content, editedAt) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const list = state.messagesByChannel[key];
      if (!list) return {};
      let changed = false;
      const next = list.map((m) => {
        if (m.id !== messageId) return m;
        changed = true;
        return { ...m, content, editedAt };
      });
      if (!changed) return {};
      return {
        messagesByChannel: { ...state.messagesByChannel, [key]: next },
      };
    }),

  removeMessageByNonce: (serverId, channelId, nonce) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const list = state.messagesByChannel[key];
      if (!list) return {};
      const next = list.filter((m) => !(m.id === 0 && m.nonce === nonce));
      if (next.length === list.length) return {};
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [key]: next,
        },
      };
    }),

  snapshotAndRemove: (serverId, channelId, messageId) => {
    const key = channelKey(serverId, channelId);
    const state = get();
    const list = state.messagesByChannel[key];
    if (!list) return undefined;
    const snap = list.find((m) => m.id === messageId);
    if (!snap) return undefined;
    set((s) => {
      const bucket = s.pendingDeletions[key] ?? new Map<number, Message>();
      const next = new Map(bucket);
      next.set(messageId, snap);
      return {
        pendingDeletions: {
          ...s.pendingDeletions,
          [key]: next,
        },
        messagesByChannel: {
          ...s.messagesByChannel,
          [key]: list.filter((m) => m.id !== messageId),
        },
      };
    });
    return snap;
  },

  restorePendingDeletion: (serverId, channelId, messageId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const bucket = state.pendingDeletions[key];
      const snap = bucket?.get(messageId);
      if (!snap) return {};
      const existing = state.messagesByChannel[key] ?? [];
      const merged = mergeMessage(existing, snap);
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[key];
      } else {
        nextPending[key] = nextBucket;
      }
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [key]: merged,
        },
        pendingDeletions: nextPending,
      };
    }),

  clearPendingDeletion: (serverId, channelId, messageId) =>
    set((state) => {
      const key = channelKey(serverId, channelId);
      const bucket = state.pendingDeletions[key];
      if (!bucket || !bucket.has(messageId)) return {};
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[key];
      } else {
        nextPending[key] = nextBucket;
      }
      return { pendingDeletions: nextPending };
    }),

  resetForLogout: () =>
    set({
      servers: [],
      discoverableServers: [],
      onlineUsers: [],
      activeServerId: null,
      activeChannelId: null,
      connectedServers: new Set(),
      pendingMembershipServerIds: new Set(),
      serverPictureVersions: {},
      serverPictures: {},
      serverMeta: {},
      serverOwner: {},
      serverPublicListing: {},
      membersByServer: {},
      bansByServer: {},
      memberRosterMeta: {},
      rolesByServer: {},
      serverAttachmentConfig: {},
      channelsByServer: {},
      messagesByChannel: {},
      pendingDeletions: {},
      hasMoreHistory: {},
      hasMoreAfter: {},
      historyLoading: {},
      historyFetched: {},
      scrollPositionsByChannel: {},
      channelAccessOrder: [],
      overwritesByChannel: {},
      invitesByServer: {},
      pendingInvite: null,
    }),

  enforceChannelCacheSize: () =>
    set((state) => {
      const cap = Math.max(1, useUiStore.getState().channelCacheSize || 10);
      if (state.channelAccessOrder.length <= cap) return {};
      // Always retain the active channel even if it's somehow not in
      // the top `cap` of the access order (defensive — shouldn't
      // happen since setActiveChannel reorders).
      const keep = state.channelAccessOrder.slice(0, cap);
      if (state.activeServerId && state.activeChannelId) {
        const activeKey = channelKey(state.activeServerId, state.activeChannelId);
        if (!keep.includes(activeKey)) {
          keep.pop();
          keep.unshift(activeKey);
        }
      }
      const keepSet = new Set<string>(keep);
      const filter = <T,>(rec: Record<ChannelKey, T>): Record<ChannelKey, T> =>
        Object.fromEntries(
          Object.entries(rec).filter(([k]) => keepSet.has(k)),
        ) as Record<ChannelKey, T>;
      return {
        channelAccessOrder: keep,
        messagesByChannel: filter(state.messagesByChannel),
        hasMoreHistory: filter(state.hasMoreHistory),
        hasMoreAfter: filter(state.hasMoreAfter),
        historyLoading: filter(state.historyLoading),
        historyFetched: filter(state.historyFetched),
        scrollPositionsByChannel: filter(state.scrollPositionsByChannel),
      };
    }),

  setScrollPosition: (serverId, channelId, topIndex, atBottom) =>
    set((state) => ({
      scrollPositionsByChannel: {
        ...state.scrollPositionsByChannel,
        [channelKey(serverId, channelId)]: { topIndex, atBottom },
      },
    })),

  setChatViewSize: (size) => set({ chatViewSize: size }),
}));
