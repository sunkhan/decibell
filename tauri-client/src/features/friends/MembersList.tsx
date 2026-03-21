import { useChatStore } from "../../stores/chatStore";

function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

export default function MembersList() {
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelMembers = useChatStore((s) => s.channelMembers);

  const members = activeChannelId
    ? channelMembers[activeChannelId] ?? []
    : [];

  return (
    <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary">
      <div className="border-b border-border px-4 py-3">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-muted">
          Online — {members.length}
        </h3>
      </div>

      <div className="flex-1 overflow-y-auto px-2 py-2">
        {members.map((username) => (
          <div
            key={username}
            className="flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-white/5"
          >
            <div className="relative flex-shrink-0">
              <div
                className="flex h-8 w-8 items-center justify-center rounded-xl text-xs font-bold text-white"
                style={{ backgroundColor: stringToColor(username) }}
              >
                {username.charAt(0).toUpperCase()}
              </div>
              <div className="absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-bg-primary bg-success" />
            </div>
            <span className="truncate text-sm text-text-primary">
              {username}
            </span>
          </div>
        ))}
        {members.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No members in this channel
          </p>
        )}
      </div>
    </div>
  );
}
