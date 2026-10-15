import { useEffect } from "react";
import { listen, type UnlistenFn } from "../../lib/ipc";
import { toUid, useUsersStore, type Uid } from "../../stores/usersStore";
import { useAuthStore } from "../../stores/authStore";
import { applyRename } from "./applyRename";

// Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md).
// The servers key every user on their central account uid, and every
// payload that names someone carries it. The renderer's stores still key
// users by name; a request that targets someone looks the uid up here
// (`uidOf`) so the server addresses the account, not whoever holds the name.
// An unknown name gives `undefined`, and the request falls back to the name.
//
// One map serves central and every community: uids are central's, so a
// name means the same account everywhere. The latest uid seen for a name
// wins. Only real accounts count (> 0); a community's placeholder for a
// name it has no uid for is addressed by name.
//
// A rename (`user_renamed`, stage 5) moves the account to its new name
// here and in every store (applyRename).

const uidByName = new Map<string, number>();
let selfUid = 0;

/// Every name → uid pair an event carried: kept here for names people type,
/// and in the users store (the reactive uid → name source) for drawing.
function rememberAll(pairs: (readonly [string | undefined, number | bigint | undefined])[]) {
  const named: [Uid, string][] = [];
  for (const [name, raw] of pairs) {
    const uid = toUid(raw);
    if (!name || !(uid > 0)) continue;
    uidByName.set(name, uid);
    named.push([uid, name]);
  }
  if (named.length) useUsersStore.getState().remember(named);
}

function remember(name: string | undefined, uid: number | bigint | undefined) {
  rememberAll([[name, uid]]);
}

/// The account a name belongs to, as far as the servers have said. For
/// names people type and settings saved before uids; everything else
/// carries the uid.
export function uidOf(name: string | null | undefined): number | undefined {
  return name ? uidByName.get(name) : undefined;
}

export { nameOf } from "../../stores/usersStore";

/// TEMPORARY bridge while the renderer moves from name keys to uid keys:
/// the uid a name belongs to (0 when unknown). Every use goes away before
/// the migration merges — grep for it.
export function legacyUid(name: string | null | undefined): number {
  return uidOf(name) ?? 0;
}

/// The media plane names people by uid in decimal (the relay stamps it as
/// sender id), so stream frames, watches and per-user volume go by this
/// key. undefined until the servers have told us the name's uid.
export function mediaKeyOf(name: string | null | undefined): string | undefined {
  const uid = uidOf(name);
  return uid === undefined ? undefined : String(uid);
}

/// Our own uid from the login response (0 before login, or against an
/// older central).
export function ownUid(): number {
  return selfUid;
}

type Named = { username?: string; uid?: number };
type Sent = { sender?: string; senderUid?: number; replyToSender?: string; replyToSenderUid?: number };

function rememberSent(list: Sent[]) {
  rememberAll(list.flatMap((m) => [[m.sender, m.senderUid], [m.replyToSender, m.replyToSenderUid]] as const));
}

/// Feeds the directory from every event that names users with their uids.
/// Mounted once with the other listen() hooks.
export function useUserDirectory() {
  useEffect(() => {
    const subs: Promise<UnlistenFn>[] = [
      listen<{ username: string; uid?: number }>("login_succeeded", ({ payload }) => {
        selfUid = toUid(payload.uid);
        remember(payload.username, payload.uid);
      }),
      listen<{ users: Named[] }>("user_list_updated", ({ payload }) => {
        rememberAll(payload.users.map((u) => [u.username, u.uid] as const));
      }),
      listen<Named>("presence_delta", ({ payload }) => remember(payload.username, payload.uid)),
      listen<{ friends: Named[] }>("friend_list_received", ({ payload }) => {
        rememberAll(payload.friends.map((f) => [f.username, f.uid] as const));
      }),
      listen<{
        conversations: { peer: string; peerUid?: number; lastMessageSender?: string; lastMessageSenderUid?: number }[];
      }>("dm_conversations_received", ({ payload }) => {
        rememberAll(
          payload.conversations.flatMap(
            (c) => [[c.peer, c.peerUid], [c.lastMessageSender, c.lastMessageSenderUid]] as const,
          ),
        );
      }),
      listen<Sent & { recipient?: string; recipientUid?: number }>("message_received", ({ payload }) => {
        rememberSent([payload]);
        remember(payload.recipient, payload.recipientUid);
      }),
      listen<{ peer: string; peerUid?: number; messages: Sent[] }>("dm_history_received", ({ payload }) => {
        remember(payload.peer, payload.peerUid);
        rememberSent(payload.messages);
      }),
      listen<{ messages: Sent[] }>("channel_history_received", ({ payload }) => {
        rememberSent(payload.messages);
      }),
      listen<{ messages?: Sent[] }>("message_search_received", ({ payload }) => {
        rememberSent(payload.messages ?? []);
      }),
      listen<{ members: Named[] }>("member_list_received", ({ payload }) => {
        rememberAll(payload.members.map((m) => [m.username, m.uid] as const));
      }),
      listen<{ members: Named[] }>("member_search_received", ({ payload }) => {
        rememberAll(payload.members.map((m) => [m.username, m.uid] as const));
      }),
      listen<{ member: Named }>("member_upsert", ({ payload }) => remember(payload.member.username, payload.member.uid)),
      listen<{ userStates?: Named[] }>("voice_presence_updated", ({ payload }) => {
        rememberAll((payload.userStates ?? []).map((st) => [st.username, st.uid] as const));
      }),
      listen<{ streams: { ownerUsername: string; ownerUid?: number }[] }>("stream_presence_updated", ({ payload }) => {
        rememberAll(payload.streams.map((st) => [st.ownerUsername, st.ownerUid] as const));
      }),
      listen<{ ownerUsername?: string; ownerUid?: number }>("community_auth_responded", ({ payload }) =>
        remember(payload.ownerUsername, payload.ownerUid),
      ),
      listen<{ ownerUsername?: string; ownerUid?: number }>("server_meta_updated", ({ payload }) =>
        remember(payload.ownerUsername, payload.ownerUid),
      ),
      listen<{ entries: Named[] }>("ban_list_received", ({ payload }) => {
        rememberAll(payload.entries.map((b) => [b.username, b.uid] as const));
      }),
      listen<{ from: string; fromUid?: number }>("call_signal", ({ payload }) => remember(payload.from, payload.fromUid)),
      listen<{ uid: number | bigint; oldUsername: string; username: string; self: boolean }>(
        "user_renamed",
        ({ payload }) => {
          const uid = toUid(payload.uid);
          if (uidByName.get(payload.oldUsername) === uid) uidByName.delete(payload.oldUsername);
          remember(payload.username, uid);
          if (payload.self) useAuthStore.setState({ username: payload.username });
          applyRename(payload.oldUsername, payload.username, payload.self);
        },
      ),
    ];
    return () => {
      for (const s of subs) s.then((off) => off()).catch(() => {});
    };
  }, []);
}
