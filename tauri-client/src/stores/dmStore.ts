import { create } from "zustand";
import type { DmMessage } from "../types";

interface DmConversation {
  username: string;
  messages: DmMessage[];
  lastMessageTime: number;
}

interface DmState {
  conversations: Record<string, DmConversation>;
  activeDmUser: string | null;
  friendsOnlyDms: boolean;

  setActiveDmUser: (username: string | null) => void;
  addDmMessage: (otherUser: string, message: DmMessage) => void;
  setFriendsOnlyDms: (value: boolean) => void;
}

export const useDmStore = create<DmState>((set) => ({
  conversations: {},
  activeDmUser: null,
  friendsOnlyDms: false,

  setActiveDmUser: (username) => set({ activeDmUser: username }),

  addDmMessage: (otherUser, message) =>
    set((state) => {
      const existing = state.conversations[otherUser];
      const timestamp = parseInt(message.timestamp, 10);
      const time = isNaN(timestamp) ? Date.now() : timestamp * 1000;

      const conversation: DmConversation = existing
        ? {
            ...existing,
            messages: [...existing.messages, message],
            lastMessageTime: time,
          }
        : {
            username: otherUser,
            messages: [message],
            lastMessageTime: time,
          };

      return {
        conversations: {
          ...state.conversations,
          [otherUser]: conversation,
        },
      };
    }),

  setFriendsOnlyDms: (value) => set({ friendsOnlyDms: value }),
}));
