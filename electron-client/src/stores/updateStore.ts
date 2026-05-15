import { create } from "zustand";

// Mirror of the main-process UpdateController state. AppLayout
// subscribes to 'update_status' broadcasts on the 'decibell:event'
// channel and pushes payloads into this store. Components elsewhere
// (AboutTab, UserPanel) read from it directly.

export type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

export type UpdateMode = "self-update" | "notify-only" | "disabled";

interface UpdateState {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
  setFromEvent: (s: UpdateStatus, m: UpdateMode, v: string) => void;
}

export const useUpdateStore = create<UpdateState>((set) => ({
  status: { state: "idle" },
  mode: "disabled",
  currentVersion: "",
  setFromEvent: (status, mode, currentVersion) =>
    set({ status, mode, currentVersion }),
}));
