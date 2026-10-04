import { invoke } from "../../../lib/ipc";
import { useAuthStore } from "../../../stores/authStore";
import { useVoiceStore } from "../../../stores/voiceStore";

// Stream watch actions for the community voice stage. They read the store at
// call time, so memoized tiles need no callback props. Several streams can
// be watched at once (each is its own subscription + decoder); "focused" is
// the one the stage shows large (`voiceStore.fullscreenStream`).

/// Subscribe to `username`'s stream if we aren't already. Self-preview is
/// renderer-internal (StreamVideoPlayer taps the local encoder), so our own
/// stream never goes over the wire.
function startWatching(username: string): boolean {
  const v = useVoiceStore.getState();
  const serverId = v.connectedServerId;
  const channelId = v.connectedChannelId;
  if (!serverId || !channelId) return false;
  if (!v.watchingStreams.includes(username)) {
    if (username !== useAuthStore.getState().username) {
      invoke("watch_stream", { serverId, channelId, targetUsername: username }).catch(() => {});
    }
    v.addWatching(username);
  }
  return true;
}

/// Watch (if needed) and show it large. Other watched streams keep playing.
export function focusStream(username: string): void {
  if (startWatching(username)) useVoiceStore.getState().setFullscreenStream(username);
}

/// Watch alongside whatever is already playing, staying on the grid.
export function watchStreamToo(username: string): void {
  startWatching(username);
}

/// Stop every other watched stream, then focus this one.
export function switchToStream(username: string): void {
  for (const other of [...useVoiceStore.getState().watchingStreams]) {
    if (other !== username) stopWatchingStream(other);
  }
  focusStream(username);
}

export function stopWatchingStream(username: string): void {
  const v = useVoiceStore.getState();
  if (username !== useAuthStore.getState().username && v.connectedServerId && v.connectedChannelId) {
    invoke("stop_watching", {
      serverId: v.connectedServerId,
      channelId: v.connectedChannelId,
      targetUsername: username,
    }).catch(() => {});
  }
  v.removeWatching(username);
}
