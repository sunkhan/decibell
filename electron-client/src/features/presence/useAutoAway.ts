// Auto-away (docs/superpowers/specs/2026-10-11-user-status-design.md): Away
// after the user's threshold without keyboard / mouse input or talking in
// voice, or on screen lock or sleep; back on activity. Idle comes from native's system_idle
// (Wayland idle-notify, GNOME's idle monitor, the screensaver's lock flag)
// or, where native has no source, Electron's powerMonitor. Central applies it only while the
// chosen status is Online, and keeps it per session (native re-reports it
// after a reconnect).

import { useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { usePresenceStore } from "../../stores/presenceStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";

/// The OS has no "input resumed" event, so the idle state is polled: slowly
/// while active (going Away may lag the threshold by this much), fast while
/// away so coming back shows within a second.
const ACTIVE_POLL_MS = 15_000;
const AWAY_POLL_MS = 1_000;

/// Talking in a voice channel or a DM call counts as activity (owner,
/// 2026-10-28): the engine's own speaking flag, which is set only while our
/// voice goes out (unmuted and over the gate's threshold, or push-to-talk
/// held over a real signal), never for the silence frames. Away then needs
/// the threshold without input and without speech. Lock and sleep still
/// mean Away at once.
function speaking(s: ReturnType<typeof useVoiceStore.getState>): boolean {
  if (!s.connectedChannelId && !s.callPeer) return false;
  return s.speakingUsers.has(useAuthStore.getState().uid);
}
/// When we last stopped (or started) speaking. Kept across threshold
/// changes, which restart the effect.
let lastSpokeAt = 0;

function report(idle: boolean) {
  if (usePresenceStore.getState().idle === idle) return;
  usePresenceStore.getState().setIdle(idle);
  invoke("set_idle", { idle }).catch(console.error);
}

export function useAutoAway() {
  const minutes = useUiStore((s) => s.awayAfterMinutes);
  const supported = usePresenceStore((s) => s.supported);

  useEffect(() => {
    if (!supported || minutes <= 0) {
      report(false);
      return;
    }
    let locked = false;
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      const threshold = minutes * 60;
      try {
        // Native first: on a Linux Wayland session Electron's powerMonitor
        // reads no idle time at all (always "active" on KDE), so the
        // compositor's idle-notify answers there. "unsupported" (Windows,
        // macOS, X11) → powerMonitor, which works on those.
        const native = await invoke<string>("system_idle_state", { thresholdSeconds: threshold }).catch(
          () => "unsupported",
        );
        const state = native === "unsupported" ? await window.decibell.idle.getState(threshold) : native;
        const spoke =
          speaking(useVoiceStore.getState()) || Date.now() - lastSpokeAt < threshold * 1000;
        if (!cancelled) report(locked || state === "locked" || (state === "idle" && !spoke));
      } catch (e) {
        console.error("[auto-away] idle state:", e);
      }
      if (cancelled) return;
      // Polls can overlap (an unlock polls at once): keep one timer chain.
      window.clearTimeout(timer);
      timer = window.setTimeout(poll, usePresenceStore.getState().idle ? AWAY_POLL_MS : ACTIVE_POLL_MS);
    };
    void poll();
    const off = window.decibell.idle.onAwayEvent((away) => {
      locked = away;
      if (away) report(true);
      void poll();
    });
    const offVoice = useVoiceStore.subscribe((s, prev) => {
      const now = speaking(s);
      if (now === speaking(prev)) return;
      // Either edge: speech starts now, or the threshold runs from here.
      lastSpokeAt = Date.now();
      // Talking brings an Away user back at once, as input does.
      if (now && usePresenceStore.getState().idle) void poll();
    });
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      off();
      offVoice();
    };
  }, [minutes, supported]);
}
