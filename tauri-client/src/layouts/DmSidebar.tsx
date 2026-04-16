import { useUiStore } from "../stores/uiStore";
import { useDmStore } from "../stores/dmStore";
import { useFriendsStore } from "../stores/friendsStore";
import { useChatStore } from "../stores/chatStore";
import { stringToGradient } from "../utils/colors";


export default function DmSidebar() {
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const conversations = useDmStore((s) => s.conversations);
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const sortedConversations = Object.values(conversations).sort(
    (a, b) => b.lastMessageTime - a.lastMessageTime
  );

  const handleDmClick = (username: string) => {
    setActiveDmUser(username);
    setActiveView("dm");
  };

  return (
    <div className="relative flex h-full w-[68px] shrink-0 flex-col items-center bg-bg-darkest pb-14 pt-2.5">
      {/* Right border — stops above UserPanel */}
      <div className="absolute right-0 top-0 bottom-14 w-px bg-border" />
      {/* DM contacts */}
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
              className={`relative flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-xl text-sm font-bold text-white transition-all duration-200 hover:-translate-y-0.5 ${
                isActive
                  ? "shadow-[0_0_0_2px_var(--color-accent)]"
                  : ""
              }`}
              style={{ background: stringToGradient(conv.username) }}
              title={conv.username}
            >
              {conv.username.charAt(0).toUpperCase()}
              <div
                className={`absolute -bottom-px -right-px h-3 w-3 rounded-full border-[2.5px] border-bg-dmbar ${
                  isOnline ? "bg-success" : "bg-text-muted"
                }`}
              />
            </button>
          );
        })}
      </div>

    </div>
  );
}
