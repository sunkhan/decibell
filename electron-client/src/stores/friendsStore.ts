import { create } from "zustand";
import type { FriendInfo } from "../types";
import type { Uid } from "./usersStore";

interface FriendsState {
  friends: FriendInfo[];
  isLoading: boolean;
  lastActionError: string | null;
  setFriends: (friends: FriendInfo[]) => void;
  setLoading: (v: boolean) => void;
  setLastActionError: (msg: string | null) => void;
  updateFriend: (uid: Uid, updates: Partial<FriendInfo>) => void;
  removeFriend: (uid: Uid) => void;
}

export const useFriendsStore = create<FriendsState>((set) => ({
  friends: [],
  isLoading: false,
  lastActionError: null,
  setFriends: (friends) => set({ friends }),
  setLoading: (v) => set({ isLoading: v }),
  setLastActionError: (msg) => set({ lastActionError: msg }),
  updateFriend: (uid, updates) =>
    set((state) => ({
      friends: state.friends.map((f) => (f.uid === uid ? { ...f, ...updates } : f)),
    })),
  removeFriend: (uid) =>
    set((state) => ({ friends: state.friends.filter((f) => f.uid !== uid) })),
}));
