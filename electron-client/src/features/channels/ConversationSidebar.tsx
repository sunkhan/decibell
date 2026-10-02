import { memo, useEffect, useMemo, useState } from "react";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore, conversationActivityTime } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { UserAvatar } from "../../components/UserAvatar";
import MessageText from "../chat/MessageText";
import { useSidebarResize } from "./useSidebarResize";

function formatRelativeTime(epochMs: number, nowMs: number): string {
  const diff = nowMs - epochMs;
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
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);

  // Sort by last-message recency. Sort itself is O(n log n) but the
  // input only changes on incoming/outgoing DMs (rare relative to
  // re-render triggers like keystrokes). Memo keeps idle re-renders
  // free.
  const sortedConversations = useMemo(
    () =>
      Object.values(conversations).sort(
        (a, b) => conversationActivityTime(b) - conversationActivityTime(a),
      ),
    [conversations],
  );
  // One lookup set instead of scanning the friend list and central's global
  // online list once per row per render.
  const onlineSet = useMemo(() => {
    const set = new Set(onlineUsers);
    for (const f of friends) if (f.status === "online") set.add(f.username);
    return set;
  }, [friends, onlineUsers]);
  // Rows are memoised, so the relative times ("5m") need their own clock.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, []);

  // Stable (setters are stable) so memo(ConversationRow) holds.
  const handleClick = useMemo(
    () => (username: string) => {
      setActiveDmUser(username);
      setActiveView("dm");
    },
    [setActiveDmUser, setActiveView],
  );

  return (
    <div
      ref={wrapperRef}
      className="relative flex shrink-0 flex-col border-r border-border bg-bg-dark pb-14"
      style={{ width }}
    >
      <div className="flex h-12 shrink-0 items-center border-b border-border px-4">
        <h2 className="font-display text-title font-emphasis tracking-title text-text-bright">
          Direct Messages
        </h2>
      </div>
      <div
        className="flex-1 overflow-y-auto px-2 py-2.5"
        style={{ "--list-row-pad-y": "8px", "--list-row-pad-x": "10px", "--list-row-gap": "10px", "--avatar-dot-size": "10px" } as React.CSSProperties}
      >
        {sortedConversations.length === 0 ? (
          <div className="flex flex-1 items-center justify-center pt-8">
            <p className="text-xs text-text-muted">No conversations yet</p>
          </div>
        ) : (
          sortedConversations.map((conv) => {
            // lastMessage is slice-independent (jump windows / trims can
            // leave messages[] ending on an older row).
            const lastMsg = conv.lastMessage ?? conv.messages[conv.messages.length - 1];
            return (
              <ConversationRow
                key={conv.username}
                username={conv.username}
                // activeDmUser is sticky across views (same pattern as
                // activeServerId) so the conversation survives a trip to
                // home — but on home the user is looking at the friends
                // page, not at it, so only highlight it in the dm view.
                isActive={activeView === "dm" && activeDmUser === conv.username}
                isOnline={onlineSet.has(conv.username)}
                unreadCount={conv.unreadCount}
                lastContent={lastMsg?.content}
                lastMessageTime={conv.lastMessageTime}
                now={now}
                onOpen={handleClick}
              />
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

// Memoised with primitive props: DM history pages and sliding-window trims
// replace `conversations` while a DM is being scrolled, and re-rendering
// every row (avatar + rich-text preview) landed on those scroll frames.
const ConversationRow = memo(function ConversationRow({
  username,
  isActive,
  isOnline,
  unreadCount,
  lastContent,
  lastMessageTime,
  now,
  onOpen,
}: {
  username: string;
  isActive: boolean;
  isOnline: boolean;
  unreadCount: number;
  lastContent: string | undefined;
  lastMessageTime: number;
  now: number;
  onOpen: (username: string) => void;
}) {
  return (
    <button
      onClick={() => onOpen(username)}
      className={`list-row flex w-full cursor-pointer items-center rounded-md transition-colors ${
        isActive
          ? "bg-accent-soft text-text-bright"
          : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
      }`}
    >
      <div className="relative shrink-0">
        <UserAvatar username={username} size={34} />
        <div
          className={`absolute -bottom-px -right-px avatar-dot rounded-full border-2 border-bg-dmbar ${
            isOnline ? "bg-success" : "bg-text-muted"
          }`}
        />
        {unreadCount > 0 && (
          <div
            className="absolute -top-1 -right-1 flex h-[18px] w-[18px] items-center justify-center rounded-full border-[2px] border-bg-dark bg-error text-[10px] font-semibold leading-none text-white"
            title={`${unreadCount} unread`}
          >
            {unreadCount > 99 ? "99+" : unreadCount}
          </div>
        )}
      </div>
      <div className="min-w-0 flex-1 text-left">
        <div className="truncate font-channel text-member font-medium">
          {username}
        </div>
        {lastContent !== undefined && (
          <div className="truncate font-channel text-[11px] font-normal text-text-muted">
            <MessageText content={lastContent} emojiSize={13} preview />
          </div>
        )}
      </div>
      {lastMessageTime > 0 && (
        <span className="shrink-0 font-channel text-[10px] font-normal text-text-muted">
          {formatRelativeTime(lastMessageTime, now)}
        </span>
      )}
    </button>
  );
});
