// Your own mic / headphones / leave, shared by every control that offers
// them (UserPanel, the voice dock, the call stage) and by global hotkeys.
// Reads the stores at call time, so callers needn't pass state in.

import { invoke } from "../../lib/ipc";
import { playSound } from "../../utils/sounds";
import { useVoiceStore } from "../../stores/voiceStore";
import { useCallStore } from "../../stores/callStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { endCall } from "../call/callActions";

/// Mic button. Unmuting while deafened also undeafens (Discord's rule).
export function toggleMute(): void {
  const { isMuted, isDeafened } = useVoiceStore.getState();
  if (isDeafened) {
    playSound("undeafen");
    invoke("set_voice_deafen", { deafened: false }).catch(console.error);
    invoke("set_voice_mute", { muted: false }).catch(console.error);
  } else {
    playSound(isMuted ? "unmute" : "mute");
    invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
  }
}

/// Headphones button.
export function toggleDeafen(): void {
  const { isDeafened } = useVoiceStore.getState();
  playSound(isDeafened ? "undeafen" : "deafen");
  invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
}

/// Leave the community voice channel.
export async function leaveVoiceChannel(): Promise<void> {
  playSound("disconnect");
  const v = useVoiceStore.getState();
  const { connectedServerId, connectedChannelId } = v;
  // If we're streaming, stop the capture/encoder and tell native to
  // stop BEFORE leaving. Otherwise capture keeps running and, since
  // disconnect() hides the dock, there's no UI left to end it.
  if (v.isStreaming) {
    const { stopActiveStream } = await import("./streaming/StreamCapture");
    await stopActiveStream();
    if (connectedServerId && connectedChannelId) {
      await invoke("stop_screen_share", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
      }).catch(console.error);
    }
    useVoiceStore.getState().setIsStreaming(false);
  }
  if (connectedServerId && connectedChannelId) {
    // Best-effort, un-awaited: leave_voice_channel below drops all watch
    // subscriptions server-side, so don't serialize N round-trips into the
    // disconnect path.
    const own = useAuthStore.getState().username;
    for (const username of useVoiceStore.getState().watchingStreams) {
      if (username !== own) {
        invoke("stop_watching", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
          targetUsername: username,
        }).catch(() => {});
      }
    }
  }
  invoke("leave_voice_channel").catch(console.error);
  useVoiceStore.getState().disconnect();
  useUiStore.getState().setActiveView("server");
}

/// The leave hotkey: hang up (or cancel) a DM call, else leave the voice
/// channel. A ringing incoming call is left alone — that's decline's job.
export async function leaveVoiceOrCall(): Promise<void> {
  const callStatus = useCallStore.getState().status;
  if (callStatus === "outgoing") {
    await endCall("Cancelled");
  } else if (callStatus === "connecting" || callStatus === "active") {
    await endCall("Call ended");
  } else if (useVoiceStore.getState().connectedChannelId != null) {
    await leaveVoiceChannel();
  }
}
