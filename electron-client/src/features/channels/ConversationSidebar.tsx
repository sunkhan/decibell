import { useMemo } from "react";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { UserAvatar } from "../../components/UserAvatar";
import MessageText from "../chat/MessageText";
import { useSidebarResize } from "./useSidebarResize";

function formatRelativeTime(epochMs: number): string {
  const diff = Date.now() - epochMs;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d`;
  return `${Math.floor(days / 30)}mo`;
}

/// DM-mode sidebar. Mounted when activeView is "home" or "dm".
/// Subscribes only to DM/friend-related slices — server-channel data
/// and voice-presence updates don't reach this component, so a
/// speaking event or channel change in another server doesn't trigger
/// a re-render here.
export default function ConversationSidebar() {
  const { wrapperRef, width, onResizeMouseDown } = useSidebarResize();

  const conversations = useDmStore((s) => s.conversations);
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);
  const setActiveView = useUiStore((s) => s.setActiveView);

  // Sort by last-message recency. Sort itself is O(n log n) but the
  // input only changes on incoming/outgoing DMs (rare relative to
  // re-render triggers like keystrokes). Memo keeps idle re-renders
  // free.
  const sortedConversations = useMemo(
    () =>
      Object.values(conversations).sort(
        (a, b) => b.lastMessageTime - a.lastMessageTime,
      ),
    [conversations],
  );

  const handleClick = (username: string) => {
    setActiveDmUser(username);
    setActiveView("dm");
  };

  return (
    <div
      ref={wrapperRef}
      className="relative flex shrink-0 flex-col border-r border-border bg-bg-dark pb-14"
      style={{ width }}
    >
      <div className="flex h-12 shrink-0 items-center border-b border-border px-4">
        <h2 className="font-display text-[15px] font-semibold text-text-bright">
          Direct Messages
        </h2>
      </div>
      <div className="flex-1 overflow-y-auto px-2 py-2.5">
        {sortedConversations.length === 0 ? (
          <div className="flex flex-1 items-center justify-center pt-8">
            <p className="text-xs text-text-muted">No conversations yet</p>
          </div>
        ) : (
          sortedConversations.map((conv) => {
            const isOnline =
              friends.some(
                (f) => f.username === conv.username && f.status === "online",
              ) || onlineUsers.includes(conv.username);
            const lastMsg = conv.messages[conv.messages.length - 1];
            const isActive = activeDmUser === conv.username;
            return (
              <button
                key={conv.username}
                onClick={() => handleClick(conv.username)}
                className={`flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-2 transition-colors ${
                  isActive
                    ? "bg-accent-soft text-text-bright"
                    : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                }`}
              >
                <div className="relative shrink-0">
                  <UserAvatar username={conv.username} size={34} />
                  <div
                    className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-dmbar ${
                      isOnline ? "bg-success" : "bg-text-muted"
                    }`}
                  />
                  {conv.unreadCount > 0 && (
                    <div
                      className="absolute -top-1 -right-1 flex h-[18px] w-[18px] items-center justify-center rounded-full border-[2px] border-bg-dark bg-error text-[9px] font-bold leading-none text-white"
                      title={`${conv.unreadCount} unread`}
                    >
                      {conv.unreadCount > 99 ? "99+" : conv.unreadCount}
                    </div>
                  )}
                </div>
                <div className="min-w-0 flex-1 text-left">
                  <div className="truncate font-channel text-[13px] font-medium">
                    {conv.username}
                  </div>
                  {lastMsg && (
                    <div className="truncate font-channel text-[11px] font-normal text-text-muted">
                      <MessageText
                        content={lastMsg.content}
                        emojiSize={13}
                        preview
                      />
                    </div>
                  )}
                </div>
                {conv.lastMessageTime > 0 && (
                  <span className="shrink-0 font-channel text-[10px] font-normal text-text-faint">
                    {formatRelativeTime(conv.lastMessageTime)}
                  </span>
                )}
              </button>
            );
          })
        )}
      </div>
      <div
        onMouseDown={onResizeMouseDown}
        className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/40 active:bg-accent/60"
      />
    </div>
  );
}
