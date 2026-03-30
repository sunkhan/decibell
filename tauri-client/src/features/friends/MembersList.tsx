import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";

export default function MembersList() {
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelMembers = useChatStore((s) => s.channelMembers);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  const members = activeChannelId
    ? channelMembers[activeChannelId] ?? []
    : [];

  return (
    <div className="flex w-[260px] shrink-0 flex-col border-l border-border bg-bg-secondary">
      <div className="px-4 pt-4 pb-2">
        <h3 className="text-[11px] font-bold uppercase tracking-[0.08em] text-text-muted">
          Online — {members.length}
        </h3>
      </div>

      <div className="flex-1 overflow-y-auto px-3 py-1">
        {members.map((username) => (
          <div
            key={username}
            className="flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-[7px] transition-colors hover:bg-surface-hover"
            onClick={(e) => {
              const rect = e.currentTarget.getBoundingClientRect();
              openProfilePopup(username, { x: rect.right + 8, y: rect.top });
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              openContextMenu(username, { x: e.clientX, y: e.clientY });
            }}
          >
            <div className="relative shrink-0">
              <div
                className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-[13px] font-bold text-white"
                style={{ background: stringToGradient(username) }}
              >
                {username.charAt(0).toUpperCase()}
              </div>
              <div className="absolute -bottom-px -right-px h-[11px] w-[11px] rounded-full border-[2.5px] border-bg-secondary bg-success" />
            </div>
            <span className="truncate text-[13.5px] font-semibold text-text-secondary">
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
