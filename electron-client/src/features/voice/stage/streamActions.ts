import { invoke } from "../../../lib/ipc";
import { useAuthStore } from "../../../stores/authStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { nameOf, type Uid } from "../../../stores/usersStore";

// Stream watch actions for the community voice stage. They read the store at
// call time, so memoized tiles need no callback props. Several streams can
// be watched at once (each is its own subscription + decoder); "focused" is
// the one the stage shows large (`voiceStore.fullscreenStream`).

/// Subscribe to `uid`'s stream if we aren't already. Self-preview is
/// renderer-internal (StreamVideoPlayer taps the local encoder), so our own
/// stream never goes over the wire.
function startWatching(uid: Uid): boolean {
  const v = useVoiceStore.getState();
  const serverId = v.connectedServerId;
  const channelId = v.connectedChannelId;
  if (!serverId || !channelId) return false;
  if (!v.watchingStreams.includes(uid)) {
    if (uid !== useAuthStore.getState().uid) {
      invoke("watch_stream", { serverId, channelId, targetUid: uid, targetUsername: nameOf(uid) }).catch(() => {});
    }
    v.addWatching(uid);
  }
  return true;
}

/// Watch (if needed) and show it large. Other watched streams keep playing.
export function focusStream(uid: Uid): void {
  if (startWatching(uid)) useVoiceStore.getState().setFullscreenStream(uid);
}

/// Watch alongside whatever is already playing, staying on the grid.
export function watchStreamToo(uid: Uid): void {
  startWatching(uid);
}

/// Stop every other watched stream, then focus this one.
export function switchToStream(uid: Uid): void {
  for (const other of [...useVoiceStore.getState().watchingStreams]) {
    if (other !== uid) stopWatchingStream(other);
  }
  focusStream(uid);
}

export function stopWatchingStream(uid: Uid): void {
  const v = useVoiceStore.getState();
  if (uid !== useAuthStore.getState().uid && v.connectedServerId && v.connectedChannelId) {
    invoke("stop_watching", {
      serverId: v.connectedServerId,
      channelId: v.connectedChannelId,
      targetUid: uid,
      targetUsername: nameOf(uid),
    }).catch(() => {});
  }
  v.removeWatching(uid);
}
