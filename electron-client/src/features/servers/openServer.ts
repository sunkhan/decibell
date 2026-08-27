import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

/// Switch the workspace to a server the way the server bar does: keep
/// the current channel when it belongs to this server, otherwise land
/// on its first text channel. When the channel list hasn't arrived yet
/// (a join in flight) there is nothing to pick; the auth response's
/// handler selects the first text channel once it lands, because the
/// server is active by then.
export function openServer(serverId: string): void {
  const chat = useChatStore.getState();
  const currentChannel = chat.activeChannelId;
  chat.setActiveServer(serverId);
  useUiStore.getState().setActiveView("server");
  const channels = chat.channelsByServer[serverId] ?? [];
  if (channels.some((ch) => ch.id === currentChannel)) return;
  // Clear first: a stale channel from another server must never sit
  // under this server, even for a commit.
  chat.setActiveChannel(null);
  const firstText = channels.find((ch) => ch.type === "text");
  if (firstText) chat.setActiveChannel(firstText.id);
}
