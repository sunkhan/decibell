import { useAuthStore } from "../../stores/authStore";
import { useAvatarStore } from "../../stores/avatarStore";
import { useCallStore } from "../../stores/callStore";
import { useChatStore } from "../../stores/chatStore";
import { useDmStore } from "../../stores/dmStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useE2eeStore } from "../../stores/e2eeStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { usePresenceStore } from "../../stores/presenceStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { saveSettings } from "../settings/saveSettings";
import { pushUserGain } from "../voice/userGain";

// A username change (uid design, stage 5). The stores key people by name
// (see "Why the client keeps name-keyed stores" in
// docs/superpowers/specs/2026-10-15-uid-identity-design.md), so a rename
// moves everything held under the old name to the new one, in place: no
// refetch, and a store that holds nothing for the name keeps its
// references (no needless re-render). Fetched lists already carry current
// names, so whatever arrives later agrees.
//
// Live voice state is left alone: a renamed user's community session
// reconnects under the new name (our own rename is refused while in a
// voice channel), and the next presence update replaces it.

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

  useAvatarStore.setState((s) => {
    const entry = s.entries.get(from);
    if (!entry) return s;
    const entries = new Map(s.entries);
    entries.delete(from);
    entries.set(to, entry);
    return { entries };
  });

  useDraftsStore.setState((s) => {
    const dmDrafts = moveKey(s.dmDrafts, from, to);
    return dmDrafts === s.dmDrafts ? s : { dmDrafts };
  });

  useE2eeStore.setState((s) => {
    const changedPeers = moveKey(s.changedPeers, from, to);
    return changedPeers === s.changedPeers ? s : { changedPeers };
  });

  useCallStore.setState((s) => {
    const peer = rename(s.peer, from, to) ?? null;
    const incoming = s.incoming && s.incoming.from === from ? { ...s.incoming, from: to } : s.incoming;
    return peer === s.peer && incoming === s.incoming ? s : { peer, incoming };
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

  // Per-user volume and local mute are saved settings: move them, save,
  // and re-apply under the new name if a call is up.
  const voice = useVoiceStore.getState();
  const hadGain = from in voice.userVolumes || voice.localMutedUsers.has(from);
  useVoiceStore.setState((s) => {
    const userVolumes = moveKey(s.userVolumes, from, to);
    let localMutedUsers = s.localMutedUsers;
    if (localMutedUsers.has(from)) {
      localMutedUsers = new Set(localMutedUsers);
      localMutedUsers.delete(from);
      localMutedUsers.add(to);
    }
    const userCapabilities = moveKey(s.userCapabilities, from, to);
    const callPeer = rename(s.callPeer, from, to) ?? null;
    return userVolumes === s.userVolumes &&
      localMutedUsers === s.localMutedUsers &&
      userCapabilities === s.userCapabilities &&
      callPeer === s.callPeer
      ? s
      : { userVolumes, localMutedUsers, userCapabilities, callPeer };
  });
  if (hadGain) {
    saveSettings();
    pushUserGain(to);
  }
}
