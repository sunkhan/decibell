// Auto-away (docs/superpowers/specs/2026-10-11-user-status-design.md): Away
// after the user's threshold without keyboard / mouse input, or at once on
// screen lock or sleep; back on activity. Central applies it only while the
// chosen status is Online, and keeps it per session (native re-reports it
// after a reconnect).

import { useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { usePresenceStore } from "../../stores/presenceStore";

/// How stale "back at the keyboard" may get: the OS has no "input resumed"
/// event, so the idle state is polled.
const POLL_MS = 15_000;

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
    const poll = async () => {
      try {
        const state = await window.decibell.idle.getState(minutes * 60);
        if (!cancelled) report(locked || state === "idle" || state === "locked");
      } catch (e) {
        console.error("[auto-away] idle state:", e);
      }
    };
    void poll();
    const timer = window.setInterval(poll, POLL_MS);
    const off = window.decibell.idle.onAwayEvent((away) => {
      locked = away;
      if (away) report(true);
      else void poll();
    });
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      off();
    };
  }, [minutes, supported]);
}
