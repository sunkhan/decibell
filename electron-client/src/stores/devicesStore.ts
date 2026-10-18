import { create } from "zustand";

/// Bumped when central says the account's device list changed
/// (`devices_changed`): an open Devices tab reloads.
interface DevicesState {
  revision: number;
  bump: () => void;
}

export const useDevicesStore = create<DevicesState>((set) => ({
  revision: 0,
  bump: () => set((s) => ({ revision: s.revision + 1 })),
}));
