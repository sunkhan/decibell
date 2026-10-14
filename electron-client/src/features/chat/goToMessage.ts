import { useChatStore } from "../../stores/chatStore";
import { openServer } from "../servers/openServer";

/// Open a channel and, when known, jump to a message in it (ChatPanel
/// picks the request up once the channel's first page is in). Used by the
/// Transfers panel and search results.
export function goToMessage(serverId: string, channelId: string, messageId = 0): void {
  openServer(serverId);
  const chat = useChatStore.getState();
  if (!chat.channelsByServer[serverId]?.some((c) => c.id === channelId)) return;
  chat.setActiveChannel(channelId);
  if (messageId > 0) chat.requestJump(serverId, channelId, messageId);
}
