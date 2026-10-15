import { useEffect } from "react";
import { listen, type UnlistenFn } from "../../lib/ipc";
import { toUid, useUsersStore, type Uid } from "../../stores/usersStore";
import { useAuthStore } from "../../stores/authStore";

// Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md).
// Every store keys people on their central account uid; this hook feeds
// the users store (uid → current name, stores/usersStore.ts) from every
// event that names someone with their uid, so drawing a name is one
// lookup and a rename (`user_renamed`) changes one entry. uids are
// central's, so one map serves central and every community. Only real
// accounts count (> 0); a community's placeholder for a legacy name is
// drawn from the record it came with.

/// Every (name, uid) pair an event carried.
function rememberAll(pairs: (readonly [string | undefined, number | bigint | undefined])[]) {
  const named: [Uid, string][] = [];
  for (const [name, raw] of pairs) {
    const uid = toUid(raw);
    if (name && uid > 0) named.push([uid, name]);
  }
  if (named.length) useUsersStore.getState().remember(named);
}

function remember(name: string | undefined, uid: number | bigint | undefined) {
  rememberAll([[name, uid]]);
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
      listen<{ username: string; uid?: number }>("login_succeeded", ({ payload }) =>
        remember(payload.username, payload.uid),
      ),
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
      listen<{ entries: { actor: string; actorUid?: number; target: string; targetUid?: number }[] }>(
        "audit_log_received",
        ({ payload }) => {
          rememberAll(
            (payload.entries ?? []).flatMap((e) => [[e.actor, e.actorUid], [e.target, e.targetUid]] as const),
          );
        },
      ),
      listen<{ invites?: { createdBy: string; createdByUid?: number; forUser?: string; forUid?: number }[] }>(
        "invite_list_received",
        ({ payload }) => {
          rememberAll(
            (payload.invites ?? []).flatMap((i) => [[i.createdBy, i.createdByUid], [i.forUser, i.forUid]] as const),
          );
        },
      ),
      listen<{ wipedBy?: string; wipedByUid?: number }>("channel_wiped", ({ payload }) =>
        remember(payload.wipedBy, payload.wipedByUid),
      ),
      listen<{ from: string; fromUid?: number }>("call_signal", ({ payload }) => remember(payload.from, payload.fromUid)),
      listen<{ uid: number | bigint; oldUsername: string; username: string; self: boolean }>(
        "user_renamed",
        ({ payload }) => {
          remember(payload.username, payload.uid);
          if (payload.self) useAuthStore.setState({ username: payload.username });
        },
      ),
    ];
    return () => {
      for (const s of subs) s.then((off) => off()).catch(() => {});
    };
  }, []);
}
