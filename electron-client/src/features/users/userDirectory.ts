import { useEffect } from "react";
import { listen, type UnlistenFn } from "../../lib/ipc";

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

const uidByName = new Map<string, number>();
let selfUid = 0;

function remember(name: string | undefined, uid: number | bigint | undefined) {
  if (!name || uid === undefined) return;
  const n = Number(uid);
  if (Number.isSafeInteger(n) && n > 0) uidByName.set(name, n);
}

/// The account a name belongs to, as far as the servers have said.
export function uidOf(name: string | null | undefined): number | undefined {
  return name ? uidByName.get(name) : undefined;
}

/// Our own uid from the login response (0 before login, or against an
/// older central).
export function ownUid(): number {
  return selfUid;
}

type Named = { username?: string; uid?: number };
type Sent = { sender?: string; senderUid?: number; replyToSender?: string; replyToSenderUid?: number };

function rememberSent(m: Sent) {
  remember(m.sender, m.senderUid);
  remember(m.replyToSender, m.replyToSenderUid);
}

/// Feeds the directory from every event that names users with their uids.
/// Mounted once with the other listen() hooks.
export function useUserDirectory() {
  useEffect(() => {
    const subs: Promise<UnlistenFn>[] = [
      listen<{ username: string; uid?: number }>("login_succeeded", ({ payload }) => {
        selfUid = Number(payload.uid ?? 0) || 0;
        remember(payload.username, payload.uid);
      }),
      listen<{ users: Named[] }>("user_list_updated", ({ payload }) => {
        for (const u of payload.users) remember(u.username, u.uid);
      }),
      listen<Named>("presence_delta", ({ payload }) => remember(payload.username, payload.uid)),
      listen<{ friends: Named[] }>("friend_list_received", ({ payload }) => {
        for (const f of payload.friends) remember(f.username, f.uid);
      }),
      listen<{
        conversations: { peer: string; peerUid?: number; lastMessageSender?: string; lastMessageSenderUid?: number }[];
      }>("dm_conversations_received", ({ payload }) => {
        for (const c of payload.conversations) {
          remember(c.peer, c.peerUid);
          remember(c.lastMessageSender, c.lastMessageSenderUid);
        }
      }),
      listen<Sent & { recipient?: string; recipientUid?: number }>("message_received", ({ payload }) => {
        rememberSent(payload);
        remember(payload.recipient, payload.recipientUid);
      }),
      listen<{ peer: string; peerUid?: number; messages: Sent[] }>("dm_history_received", ({ payload }) => {
        remember(payload.peer, payload.peerUid);
        payload.messages.forEach(rememberSent);
      }),
      listen<{ messages: Sent[] }>("channel_history_received", ({ payload }) => {
        payload.messages.forEach(rememberSent);
      }),
      listen<{ messages?: Sent[] }>("message_search_received", ({ payload }) => {
        payload.messages?.forEach(rememberSent);
      }),
      listen<{ members: Named[] }>("member_list_received", ({ payload }) => {
        for (const m of payload.members) remember(m.username, m.uid);
      }),
      listen<{ members: Named[] }>("member_search_received", ({ payload }) => {
        for (const m of payload.members) remember(m.username, m.uid);
      }),
      listen<{ member: Named }>("member_upsert", ({ payload }) => remember(payload.member.username, payload.member.uid)),
      listen<{ userStates?: Named[] }>("voice_presence_updated", ({ payload }) => {
        for (const s of payload.userStates ?? []) remember(s.username, s.uid);
      }),
      listen<{ streams: { ownerUsername: string; ownerUid?: number }[] }>("stream_presence_updated", ({ payload }) => {
        for (const st of payload.streams) remember(st.ownerUsername, st.ownerUid);
      }),
      listen<{ ownerUsername?: string; ownerUid?: number }>("community_auth_responded", ({ payload }) =>
        remember(payload.ownerUsername, payload.ownerUid),
      ),
      listen<{ ownerUsername?: string; ownerUid?: number }>("server_meta_updated", ({ payload }) =>
        remember(payload.ownerUsername, payload.ownerUid),
      ),
      listen<{ entries: Named[] }>("ban_list_received", ({ payload }) => {
        for (const b of payload.entries) remember(b.username, b.uid);
      }),
      listen<{ from: string; fromUid?: number }>("call_signal", ({ payload }) => remember(payload.from, payload.fromUid)),
    ];
    return () => {
      for (const s of subs) s.then((off) => off()).catch(() => {});
    };
  }, []);
}
