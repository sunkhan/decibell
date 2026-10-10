import { memo, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { UserAvatar } from "../../components/UserAvatar";
import { EMPTY_LIST } from "../../lib/empty";
import type { ServerMember } from "../../types";

// One collator for every sort: localeCompare without one rebuilds the ICU
// collator per comparison.
const collator = new Intl.Collator();
const displayNameOf = (m: ServerMember) => m.nickname || m.username;
const byName = (a: ServerMember, b: ServerMember) =>
  collator.compare(displayNameOf(a), displayNameOf(b));

export default function MembersList() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  // This server's roster only — the whole map changes on presence deltas in
  // every connected server.
  const roster = useChatStore((s): ServerMember[] =>
    activeServerId ? s.membersByServer[activeServerId] ?? EMPTY_LIST : EMPTY_LIST,
  );
  const rosterMeta = useChatStore((s) =>
    activeServerId ? s.memberRosterMeta[activeServerId] : undefined,
  );
  const setMembersLoadingMore = useChatStore((s) => s.setMembersLoadingMore);

  // Roster paging: page 1 (every online member + the first offline page)
  // is fetched on auth; further offline pages load as the list scrolls
  // to its end. Live changes arrive as deltas, so no polling.
  const loadMore = useCallback(() => {
    if (!activeServerId || !rosterMeta?.hasMore || rosterMeta.loadingMore) return;
    setMembersLoadingMore(activeServerId, true);
    invoke("list_members", {
      serverId: activeServerId,
      after: rosterMeta.nextAfter,
      limit: 100,
    }).catch((err) => {
      console.error("list_members:", err);
      setMembersLoadingMore(activeServerId, false);
    });
  }, [activeServerId, rosterMeta, setMembersLoadingMore]);

  const sentinelRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) loadMore();
      },
      { rootMargin: "200px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [loadMore]);

  const { online, offline } = useMemo(() => {
    const onlineList = roster.filter((m) => m.isOnline);
    const offlineList = roster.filter((m) => !m.isOnline);
    // Sort by display name (nickname when set), matching how the list reads.
    onlineList.sort(byName);
    offlineList.sort(byName);
    return { online: onlineList, offline: offlineList };
  }, [roster]);

  const renderRow = (m: ServerMember, isOnline: boolean) => (
    <MemberRow key={m.username} member={m} isOnline={isOnline} serverId={activeServerId} />
  );

  return (
    <div className="flex w-[260px] shrink-0 flex-col border-l border-border bg-bg-dark">
      <div
        className="flex-1 overflow-y-auto px-3 pt-1 pb-3"
        style={{ "--list-row-pad-y": "7px", "--list-row-pad-x": "8px", "--list-row-gap": "10px" } as React.CSSProperties}
      >
        <div className="px-1 pt-3 pb-1">
          <h3 className="font-section text-section font-medium uppercase leading-none tracking-section text-text-muted">
            Online — {online.length}
          </h3>
        </div>
        {online.map((m) => renderRow(m, true))}

        {(offline.length > 0 || rosterMeta?.hasMore) && (
          <>
            <div className="px-1 pt-4 pb-1">
              <h3 className="font-section text-section font-medium uppercase leading-none tracking-section text-text-muted">
                Offline — {rosterMeta ? Math.max(0, rosterMeta.totalMembers - online.length) : offline.length}
              </h3>
            </div>
            {offline.map((m) => renderRow(m, false))}
            {rosterMeta?.hasMore && (
              <div ref={sentinelRef} className="px-1 py-2">
                <button
                  type="button"
                  onClick={loadMore}
                  disabled={rosterMeta.loadingMore}
                  className="w-full rounded-sm py-1.5 text-[12px] text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary disabled:cursor-default"
                >
                  {rosterMeta.loadingMore ? "Loading…" : "Load more"}
                </button>
              </div>
            )}
          </>
        )}

        {online.length === 0 && offline.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No members yet
          </p>
        )}
      </div>
    </div>
  );
}

// Memoised: a roster delta replaces only the changed member's object, so a
// presence flip re-renders that one row instead of the whole list.
const MemberRow = memo(function MemberRow({
  member: m,
  isOnline,
  serverId,
}: {
  member: ServerMember;
  isOnline: boolean;
  serverId: string | null;
}) {
  return (
    <div
      className="list-row group flex cursor-pointer items-center rounded-sm transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        useUiStore
          .getState()
          .openProfilePopup(m.username, { x: rect.right + 8, y: rect.top }, serverId);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        useUiStore
          .getState()
          .openContextMenu(m.username, { x: e.clientX, y: e.clientY }, serverId);
      }}
    >
      <div className={`relative shrink-0 ${isOnline ? "" : "opacity-[0.62]"}`}>
        <UserAvatar username={m.username} size={34} />
        <div
          className={`absolute -bottom-px -right-px avatar-dot rounded-full border-[2.5px] border-bg-tertiary ${
            isOnline ? "bg-success" : "bg-text-muted"
          }`}
        />
      </div>
      <span
        className={`truncate font-channel text-member transition-colors ${
          isOnline
            ? "font-medium text-text-secondary group-hover:text-text-primary"
            : "font-normal text-text-muted group-hover:text-text-primary"
        }`}
      >
        {m.nickname || m.username}
      </span>
    </div>
  );
});
