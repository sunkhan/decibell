import { memo, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import {
  isOnlineStatus,
  resolvePresence,
  usePresenceStore,
  useStatusMessage,
  type DisplayStatus,
} from "../../stores/presenceStore";
import { UserAvatar } from "../../components/UserAvatar";
import { StatusDot } from "../../components/StatusDot";
import { StatusMessageText } from "../../components/StatusMessageText";
import { EMPTY_LIST } from "../../lib/empty";
import type { ServerMember } from "../../types";
import { useUserName, useUsersStore, type Uid } from "../../stores/usersStore";

// One collator for every sort: localeCompare without one rebuilds the ICU
// collator per comparison.
const collator = new Intl.Collator();

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

  // Status comes from central (presenceStore); the community's own
  // isOnline only stands in until central's first snapshot. Away and Do
  // Not Disturb are online.
  const me = useAuthStore((s) => s.uid);
  // Current names (nickname first), so a rename re-sorts the list.
  const names = useUsersStore((s) => s.names);
  const statuses = usePresenceStore((s) => s.statuses);
  const loaded = usePresenceStore((s) => s.loaded);
  const ownStatus = usePresenceStore((s) => s.ownStatus);
  const idle = usePresenceStore((s) => s.idle);
  const { online, offline, statusOf } = useMemo(() => {
    const presence = { statuses, loaded, ownStatus, idle };
    const statusOf = new Map<Uid, DisplayStatus>();
    for (const m of roster) statusOf.set(m.uid, resolvePresence(presence, me, m.uid, m.isOnline));
    const onlineList = roster.filter((m) => isOnlineStatus(statusOf.get(m.uid)!));
    const offlineList = roster.filter((m) => !isOnlineStatus(statusOf.get(m.uid)!));
    const shown = (m: ServerMember) => m.nickname || (m.uid ? names.get(m.uid) : undefined) || m.username;
    const byName = (a: ServerMember, b: ServerMember) => collator.compare(shown(a), shown(b));
    // Sort by display name (nickname when set), matching how the list reads.
    onlineList.sort(byName);
    offlineList.sort(byName);
    return { online: onlineList, offline: offlineList, statusOf };
  }, [roster, statuses, loaded, ownStatus, idle, me, names]);

  const renderRow = (m: ServerMember) => (
    <MemberRow
      key={m.uid}
      member={m}
      status={statusOf.get(m.uid) ?? "offline"}
      serverId={activeServerId}
    />
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
        {online.map(renderRow)}

        {(offline.length > 0 || rosterMeta?.hasMore) && (
          <>
            <div className="px-1 pt-4 pb-1">
              <h3 className="font-section text-section font-medium uppercase leading-none tracking-section text-text-muted">
                Offline — {rosterMeta ? Math.max(0, rosterMeta.totalMembers - online.length) : offline.length}
              </h3>
            </div>
            {offline.map(renderRow)}
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
  status,
  serverId,
}: {
  member: ServerMember;
  status: DisplayStatus;
  serverId: string | null;
}) {
  const isOnline = isOnlineStatus(status);
  const uid = m.uid;
  const name = useUserName(uid, m.username);
  const message = useStatusMessage(uid);
  return (
    <div
      className="list-row group flex cursor-pointer items-center rounded-sm transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        useUiStore
          .getState()
          .openProfilePopup(uid, { x: rect.right + 8, y: rect.top }, serverId);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        useUiStore
          .getState()
          .openContextMenu(uid, { x: e.clientX, y: e.clientY }, serverId);
      }}
    >
      <div className={`relative shrink-0 ${isOnline ? "" : "opacity-[0.62]"}`}>
        <UserAvatar uid={uid} name={name} size={34} />
        <StatusDot
          status={status}
          ring="var(--color-bg-tertiary)"
          className="avatar-dot absolute -bottom-px -right-px"
        />
      </div>
      <div className="min-w-0 flex-1">
        <div
          className={`truncate font-channel text-member leading-4.5 transition-colors ${
            isOnline
              ? "font-medium text-text-secondary group-hover:text-text-primary"
              : "font-normal text-text-muted group-hover:text-text-primary"
          }`}
        >
          {m.nickname || name}
        </div>
        {message && (
          <StatusMessageText message={message} emojiSize={13} className="font-meta text-micro leading-4 text-text-muted" />
        )}
      </div>
    </div>
  );
});
