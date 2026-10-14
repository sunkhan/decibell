import { createContext, useContext, useEffect } from "react";

/// Server settings tabs each keep their own draft, and the window asks
/// the open tab before it closes or changes tab. A tab with unsaved
/// changes reports them here; a refused leave bumps `nudge`, which shakes
/// that tab's save bar. Unsaved edits are never dropped silently.
export const LeaveGuardContext = createContext<{
  nudge: number;
  setDirty: (dirty: boolean) => void;
} | null>(null);

/// Reports this tab's unsaved changes to the window. Returns how many
/// leaves have been refused since they began.
export function useLeaveGuard(dirty: boolean): number {
  const guard = useContext(LeaveGuardContext);
  const setDirty = guard?.setDirty;
  useEffect(() => {
    if (!setDirty) return;
    setDirty(dirty);
    return () => setDirty(false);
  }, [setDirty, dirty]);
  return guard?.nudge ?? 0;
}
