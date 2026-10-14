import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import ChannelEncryptionBadge from "../e2ee/ChannelEncryptionBadge";
import SearchBox from "../search/SearchBox";

/// A text channel's header: name, encryption badge, topic, the search box
/// and the members toggle. It spans the chat and the panel on its right
/// (members or search results), which start under it, as in Discord —
/// MainLayout stacks it above both. The height stays 48 px (h-12): the
/// chat's media sizing and the floating search panel assume it.
export default function ChannelHeader() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channel = useChatStore((s) =>
    s.activeServerId && s.activeChannelId
      ? s.channelsByServer[s.activeServerId]?.find((c) => c.id === s.activeChannelId) ?? null
      : null,
  );
  if (!activeServerId || !activeChannelId) return null;
  const channelName = channel?.name ?? activeChannelId;
  return (
    <div className="flex h-12 shrink-0 items-center border-b border-border-divider px-4 font-display text-title font-emphasis tracking-title text-text-bright">
      <span className="mr-1.5 text-text-muted">#</span>
      <span className="min-w-0 truncate">{channelName}</span>
      {channel?.encrypted ? (
        <span className="ml-2.5 flex items-center">
          <ChannelEncryptionBadge />
        </span>
      ) : null}
      {channel?.topic ? (
        <>
          <span aria-hidden className="mx-3 h-5 w-px shrink-0 bg-border-divider" />
          {/* basis-0: the topic takes only what the name leaves; contained
              so a long one never widens the header's intrinsic size. */}
          <span
            title={channel.topic}
            className="mr-3 min-w-0 flex-1 basis-0 truncate [contain:inline-size] font-channel text-[13px] font-normal tracking-normal text-text-muted"
          >
            {channel.topic}
          </span>
        </>
      ) : null}
      {/* The members toggle, then the search box at the right end. */}
      <MembersPanelToggle />
      <SearchBox serverId={activeServerId} channelId={activeChannelId} />
    </div>
  );
}

/// Header button that shows / hides the members list beside a text
/// channel (the friends toggle in a DM header uses the same icon). Its own
/// component so the toggle re-renders this button, not the header.
/// Visibility is remembered per install by uiStore. On looks the same as
/// off (owner, 2026-10-14): the open list shows the state; aria-pressed
/// still reports it.
function MembersPanelToggle() {
  const visible = useUiStore((s) => s.membersPanelVisible);
  const toggle = useUiStore((s) => s.toggleMembersPanel);
  return (
    <button
      onClick={toggle}
      aria-pressed={visible}
      className="ml-auto flex h-8 w-8 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
      title={visible ? "Hide members" : "Show members"}
    >
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />
      </svg>
    </button>
  );
}
