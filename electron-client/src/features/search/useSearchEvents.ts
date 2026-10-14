import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import type { MessageSearchReceivedPayload } from "../../types";
import { mapStoredMessage } from "../chat/useChatEvents";
import { useSearchStore } from "./searchStore";

/// Feeds MESSAGE_SEARCH_RES into the search store. Mounted once, next to
/// useChatEvents.
export function useSearchEvents() {
  useEffect(() => {
    const un = listen<MessageSearchReceivedPayload>("message_search_received", (event) => {
      const p = event.payload;
      useSearchStore.getState().receive({
        serverId: p.serverId,
        seq: Number(p.seq),
        success: p.success,
        message: p.message,
        messages: (p.messages ?? []).map((m) => mapStoredMessage(p.serverId, m)),
        hasMore: p.hasMore,
        // i64s can arrive as BigInt past 2^31.
        nextCursor: Number(p.nextCursor),
        nextOffset: Number(p.nextOffset),
        total: Number(p.total),
        totalCapped: p.totalCapped,
        textSkippedChannelIds: p.textSkippedChannelIds ?? [],
        partial: p.partial,
        retryAfterMs: Number(p.retryAfterMs),
        firstPage: p.firstPage,
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);
}
