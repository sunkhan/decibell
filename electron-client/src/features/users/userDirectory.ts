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
//
// Central's names are current and win. A community's can be older (it
// learns a rename when the member next connects to it), so they only fill
// in a uid central hasn't named. Names kept as history (ban rows, audit
// entries, invite labels, wipes) don't feed the store at all: those views
// draw their own payload name as the fallback.

type Pair = readonly [string | undefined, number | bigint | undefined];

function rememberAll(pairs: Pair[], fromCentral: boolean) {
  const named: [Uid, string][] = [];
  for (const [name, raw] of pairs) {
    const uid = toUid(raw);
    if (name && uid > 0) named.push([uid, name]);
  }
  if (named.length) useUsersStore.getState().remember(named, fromCentral);
}

const central = (pairs: Pair[]) => rememberAll(pairs, true);
const community = (pairs: Pair[]) => rememberAll(pairs, false);

type Named = { username?: string; uid?: number };
type Sent = { sender?: string; senderUid?: number; replyToSender?: string; replyToSenderUid?: number };

const sentPairs = (list: Sent[]): Pair[] =>
  list.flatMap((m) => [[m.sender, m.senderUid], [m.replyToSender, m.replyToSenderUid]] as const);

/// Feeds the directory from every event that names users with their uids.
/// Mounted once with the other listen() hooks.
export function useUserDirectory() {
  useEffect(() => {
    const subs: Promise<UnlistenFn>[] = [
      // Central.
      listen<{ username: string; uid?: number }>("login_succeeded", ({ payload }) =>
        central([[payload.username, payload.uid]]),
      ),
      listen<{ users: Named[] }>("user_list_updated", ({ payload }) => {
        central(payload.users.map((u) => [u.username, u.uid] as const));
      }),
      listen<Named>("presence_delta", ({ payload }) => central([[payload.username, payload.uid]])),
      listen<{ friends: Named[] }>("friend_list_received", ({ payload }) => {
        central(payload.friends.map((f) => [f.username, f.uid] as const));
      }),
      listen<{
        conversations: { peer: string; peerUid?: number; lastMessageSender?: string; lastMessageSenderUid?: number }[];
      }>("dm_conversations_received", ({ payload }) => {
        central(
          payload.conversations.flatMap(
            (c) => [[c.peer, c.peerUid], [c.lastMessageSender, c.lastMessageSenderUid]] as const,
          ),
        );
      }),
      listen<Sent & { context?: string; recipient?: string; recipientUid?: number }>("message_received", ({ payload }) => {
        // DMs come from central; channel messages from a community.
        if (payload.context === "dm") central([...sentPairs([payload]), [payload.recipient, payload.recipientUid]]);
        else community(sentPairs([payload]));
      }),
      listen<{ peer: string; peerUid?: number; messages: Sent[] }>("dm_history_received", ({ payload }) => {
        central([[payload.peer, payload.peerUid], ...sentPairs(payload.messages)]);
      }),
      listen<{ from: string; fromUid?: number }>("call_signal", ({ payload }) => central([[payload.from, payload.fromUid]])),
      listen<{ uid: number | bigint; username: string; self: boolean }>(
        "user_renamed",
        ({ payload }) => {
          central([[payload.username, payload.uid]]);
          if (payload.self) useAuthStore.setState({ username: payload.username });
        },
      ),
      // Communities.
      listen<{ messages: Sent[] }>("channel_history_received", ({ payload }) => community(sentPairs(payload.messages))),
      listen<{ messages?: Sent[] }>("message_search_received", ({ payload }) => community(sentPairs(payload.messages ?? []))),
      listen<{ members: Named[]; extra?: Named[] }>("member_list_received", ({ payload }) => {
        community([...payload.members, ...(payload.extra ?? [])].map((m) => [m.username, m.uid] as const));
      }),
      listen<{ members: Named[] }>("member_search_received", ({ payload }) => {
        community(payload.members.map((m) => [m.username, m.uid] as const));
      }),
      listen<{ member: Named }>("member_upsert", ({ payload }) => community([[payload.member.username, payload.member.uid]])),
      listen<{ userStates?: Named[] }>("voice_presence_updated", ({ payload }) => {
        community((payload.userStates ?? []).map((st) => [st.username, st.uid] as const));
      }),
      listen<{ streams: { ownerUsername: string; ownerUid?: number }[] }>("stream_presence_updated", ({ payload }) => {
        community(payload.streams.map((st) => [st.ownerUsername, st.ownerUid] as const));
      }),
      listen<{ ownerUsername?: string; ownerUid?: number }>("community_auth_responded", ({ payload }) =>
        community([[payload.ownerUsername, payload.ownerUid]]),
      ),
      listen<{ ownerUsername?: string; ownerUid?: number }>("server_meta_updated", ({ payload }) =>
        community([[payload.ownerUsername, payload.ownerUid]]),
      ),
    ];
    return () => {
      for (const s of subs) s.then((off) => off()).catch(() => {});
    };
  }, []);
}
