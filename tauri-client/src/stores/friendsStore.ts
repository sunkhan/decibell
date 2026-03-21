import { create } from "zustand";
import type { FriendInfo } from "../types";

interface FriendsState {
  friends: FriendInfo[];
  isLoading: boolean;
  lastActionError: string | null;
  setFriends: (friends: FriendInfo[]) => void;
  setLoading: (v: boolean) => void;
  setLastActionError: (msg: string | null) => void;
  updateFriend: (username: string, updates: Partial<FriendInfo>) => void;
  removeFriend: (username: string) => void;
}

export const useFriendsStore = create<FriendsState>((set) => ({
  friends: [], isLoading: false, lastActionError: null,
  setFriends: (friends) => set({ friends }),
  setLoading: (v) => set({ isLoading: v }),
  setLastActionError: (msg) => set({ lastActionError: msg }),
  updateFriend: (username, updates) => set((state) => ({ friends: state.friends.map((f) => f.username === username ? { ...f, ...updates } : f) })),
  removeFriend: (username) => set((state) => ({ friends: state.friends.filter((f) => f.username !== username) })),
}));
