import { useUiStore } from "../stores/uiStore";
import { useDmStore } from "../stores/dmStore";
import { useFriendsStore } from "../stores/friendsStore";
import { useChatStore } from "../stores/chatStore";
import { UserAvatar } from "../components/UserAvatar";

// Vertical column on the far left (between the bottom of the
// horizontal ServerBar and the top of the floating UserPanel). Lists
// active DM conversations sorted by most-recent activity. PR4-parity
// stage: friends/DM events aren't listened to yet, so this renders
// an empty list — the visual frame still matches tauri-client.
export default function DmSidebar() {
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const conversations = useDmStore((s) => s.conversations);
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const sortedConversations = Object.values(conversations).sort(
    (a, b) => b.lastMessageTime - a.lastMessageTime,
  );

  const handleDmClick = (username: string) => {
    setActiveDmUser(username);
    setActiveView("dm");
  };

  return (
    <div className="relative flex h-full w-[68px] shrink-0 flex-col items-center bg-bg-darkest pb-14 pt-px">
      <div className="absolute right-0 top-0 bottom-14 w-px bg-border" />
      <div className="flex flex-1 flex-col items-center gap-1.5 overflow-y-auto px-3 py-1">
        {sortedConversations.map((conv) => {
          const isOnline =
            friends.some((f) => f.username === conv.username && f.status === "online") ||
            onlineUsers.includes(conv.username);
          const isActive = activeDmUser === conv.username && activeView === "dm";
          return (
            <button
              key={conv.username}
              onClick={() => handleDmClick(conv.username)}
              className={`relative shrink-0 rounded-md transition-all duration-200 hover:-translate-y-0.5 ${
                isActive ? "shadow-[0_0_0_2px_var(--color-accent)]" : ""
              }`}
              title={conv.username}
            >
              <UserAvatar username={conv.username} size={38} />
              <div
                className={`absolute -bottom-px -right-px h-3 w-3 rounded-full border-[2.5px] border-bg-dmbar ${
                  isOnline ? "bg-success" : "bg-text-muted"
                }`}
              />
              {conv.unreadCount > 0 && (
                <div
                  className="absolute -top-px -right-px flex h-[16px] min-w-[16px] items-center justify-center rounded-full border-[2px] border-bg-darkest bg-error px-1 text-[9px] font-bold text-white"
                  title={`${conv.unreadCount} unread`}
                >
                  {conv.unreadCount > 99 ? "99+" : conv.unreadCount}
                </div>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
