import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";

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

}
