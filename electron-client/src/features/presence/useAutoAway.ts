// Auto-away (docs/superpowers/specs/2026-10-11-user-status-design.md): Away
// after the user's threshold without keyboard / mouse input, or on screen
// lock or sleep; back on activity. Idle comes from native's system_idle
// (Wayland idle-notify, GNOME's idle monitor, the screensaver's lock flag)
// or, where native has no source, Electron's powerMonitor. Central applies it only while the
// chosen status is Online, and keeps it per session (native re-reports it
// after a reconnect).

import { useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { usePresenceStore } from "../../stores/presenceStore";

/// The OS has no "input resumed" event, so the idle state is polled: slowly
/// while active (going Away may lag the threshold by this much), fast while
/// away so coming back shows within a second.
const ACTIVE_POLL_MS = 15_000;
const AWAY_POLL_MS = 1_000;

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
        if (!cancelled) report(locked || state === "idle" || state === "locked");
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
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      off();
    };
  }, [minutes, supported]);
}
