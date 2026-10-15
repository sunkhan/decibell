import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useDmStore } from "../../stores/dmStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useE2eeStore } from "../../stores/e2eeStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { usePresenceStore } from "../../stores/presenceStore";
import { useUiStore } from "../../stores/uiStore";

// A username change (uid design, stage 5). The stores key people by name
// (see "Why the client keeps name-keyed stores" in
// docs/superpowers/specs/2026-10-15-uid-identity-design.md), so a rename
// moves everything held under the old name to the new one, in place: no
// refetch, and a store that holds nothing for the name keeps its
// references (no needless re-render). Fetched lists already carry current
// names, so whatever arrives later agrees.
//
// Voice and streams carry on through a rename (the media plane names people
// by uid, and the community renames a live session in place), so their
// name-keyed state moves too: rosters, speaking, the streams we watch or
// show, thumbnails. The community's next presence update agrees.

/// `rec` with key `from` moved to `to`; the same object when absent.
function moveKey<V>(rec: Record<string, V>, from: string, to: string): Record<string, V> {
  if (!(from in rec)) return rec;
  const next = { ...rec };
  next[to] = next[from];
  delete next[from];
  return next;
}

function rename(name: string | null | undefined, from: string, to: string): string | null | undefined {
  return name === from ? to : name;
}

/// Messages with `from` as sender or reply sender renamed; the same array
/// when none match.
function renameSenders<M extends { sender: string; replyToSender?: string }>(
  list: M[],
  from: string,
  to: string,
): M[] {
  if (!list.some((m) => m.sender === from || m.replyToSender === from)) return list;
  return list.map((m) =>
    m.sender === from || m.replyToSender === from
      ? { ...m, sender: m.sender === from ? to : m.sender, replyToSender: m.replyToSender === from ? to : m.replyToSender }
      : m,
  );
}

export function applyRename(from: string, to: string, self: boolean): void {
  if (!from || !to || from === to) return;

  if (self) useAuthStore.setState({ username: to });

  useDmStore.setState((s) => {
    let conversations = s.conversations;
    // Messages in every thread: ours carry our name wherever they are.
    for (const [peer, conv] of Object.entries(conversations)) {
      const messages = renameSenders(conv.messages, from, to);
      if (messages !== conv.messages) {
        if (conversations === s.conversations) conversations = { ...conversations };
        conversations[peer] = { ...conv, messages };
      }
    }
    if (from in conversations) {
      conversations = moveKey(conversations, from, to);
      conversations[to] = { ...conversations[to], username: to };
    }
    const pendingDmDeletions = moveKey(s.pendingDmDeletions, from, to);
    const activeDmUser = rename(s.activeDmUser, from, to) ?? null;
    if (
      conversations === s.conversations &&
      pendingDmDeletions === s.pendingDmDeletions &&
      activeDmUser === s.activeDmUser
    ) {
      return s;
    }
    return { conversations, pendingDmDeletions, activeDmUser };
  });

  useChatStore.setState((s) => {
    let messagesByChannel = s.messagesByChannel;
    for (const [key, list] of Object.entries(messagesByChannel)) {
      const next = renameSenders(list, from, to);
      if (next !== list) {
        if (messagesByChannel === s.messagesByChannel) messagesByChannel = { ...messagesByChannel };
        messagesByChannel[key as keyof typeof messagesByChannel] = next;
      }
    }
    let membersByServer = s.membersByServer;
    for (const [sid, members] of Object.entries(membersByServer)) {
      if (!members.some((m) => m.username === from)) continue;
      if (membersByServer === s.membersByServer) membersByServer = { ...membersByServer };
      membersByServer[sid] = members.map((m) => (m.username === from ? { ...m, username: to } : m));
    }
    let serverOwner = s.serverOwner;
    for (const [sid, owner] of Object.entries(serverOwner)) {
      if (owner !== from) continue;
      if (serverOwner === s.serverOwner) serverOwner = { ...serverOwner };
      serverOwner[sid] = to;
    }
    if (
      messagesByChannel === s.messagesByChannel &&
      membersByServer === s.membersByServer &&
      serverOwner === s.serverOwner
    ) {
      return s;
    }
    return { messagesByChannel, membersByServer, serverOwner };
  });

  useFriendsStore.setState((s) =>
    s.friends.some((f) => f.username === from)
      ? { friends: s.friends.map((f) => (f.username === from ? { ...f, username: to } : f)) }
      : s,
  );

  usePresenceStore.setState((s) => {
    const statuses = moveKey(s.statuses, from, to);
    const messages = moveKey(s.messages, from, to);
    return statuses === s.statuses && messages === s.messages ? s : { statuses, messages };
  });

  useDraftsStore.setState((s) => {
    const dmDrafts = moveKey(s.dmDrafts, from, to);
    return dmDrafts === s.dmDrafts ? s : { dmDrafts };
  });

  useE2eeStore.setState((s) => {
    const changedPeers = moveKey(s.changedPeers, from, to);
    return changedPeers === s.changedPeers ? s : { changedPeers };
  });

  useUiStore.setState((s) => {
    const userProfileUser = rename(s.userProfileUser, from, to) ?? null;
    const profilePopupUser = rename(s.profilePopupUser, from, to) ?? null;
    const contextMenuUser = rename(s.contextMenuUser, from, to) ?? null;
    return userProfileUser === s.userProfileUser &&
      profilePopupUser === s.profilePopupUser &&
      contextMenuUser === s.contextMenuUser
      ? s
      : { userProfileUser, profilePopupUser, contextMenuUser };
  });

}
