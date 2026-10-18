import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
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
import { useUserName } from "../../stores/usersStore";
import { ROSTER_PAGE, visibleRoster } from "../members/rosterOrder";
import {
  memberListLayout,
  memberListWindow,
  nearLoadedEnd,
  windowRange,
  type MemberListMetrics,
} from "../members/memberWindow";

const AVATAR_PX = 34;

// The list's row metrics. Every member row is the same whole-pixel height,
// the avatar's plus the row padding, scaled by the List density setting,
// with or without a status message: the window's arithmetic (memberWindow.ts)
// needs one height per kind of item. The name and status lines are as tall
// as the avatar at the default density; below it they reach a little into
// the row's padding.
const LIST_VARS = {
  "--list-row-pad-y": "7px",
  "--list-row-pad-x": "8px",
  "--list-row-gap": "10px",
  "--member-row-h": `round(calc((${AVATAR_PX}px + 2 * var(--list-row-pad-y)) * var(--ui-row-scale)), 1px)`,
} as React.CSSProperties;

const ONLINE_HEADER_CLASS = "flex h-7 items-end px-1 pb-1";
const OFFLINE_HEADER_CLASS = "flex h-8 items-end px-1 pb-1";
const HEADING_CLASS = "font-section text-section font-medium uppercase leading-none tracking-section text-text-muted";

/// Until the probe is measured (the first layout effect, before paint).
const DEFAULT_METRICS: MemberListMetrics = { row: 48, onlineHeader: 28, offlineHeader: 32 };

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

  // Roster paging ("Roster protocol" in messages.proto): page 1 is fetched
  // on auth; the pages run through the online members, then the offline
  // ones, and the next loads as the list scrolls to the end of what's
  // loaded. Live changes arrive as deltas, so no polling.
  // Reads the store, not the render: scroll events can call it again before
  // the render that saw the first call.
  const loadMore = useCallback(() => {
    if (!activeServerId) return;
    const meta = useChatStore.getState().memberRosterMeta[activeServerId];
    if (!meta?.hasMore || meta.loadingMore || meta.refetching) return;
    setMembersLoadingMore(activeServerId, true);
    invoke("list_members", {
      serverId: activeServerId,
      after: meta.nextAfter,
      limit: ROSTER_PAGE,
    }).catch((err) => {
      console.error("list_members:", err);
      setMembersLoadingMore(activeServerId, false);
    });
  }, [activeServerId, setMembersLoadingMore]);

  // Which part a member is in, and the counts, are the community's
  // (MemberInfo.isOnline: connected and not invisible), so the headers stay
  // exact while only part of the list is loaded. Central's status refines
  // the dot inside the online part (away, Do Not Disturb), and stands in
  // nowhere else: an online member central hasn't heard from shows online,
  // one central has online but who isn't connected here shows offline.
  const me = useAuthStore((s) => s.uid);
  const statuses = usePresenceStore((s) => s.statuses);
  const known = usePresenceStore((s) => s.known);
  const ownStatus = usePresenceStore((s) => s.ownStatus);
  const accountIdle = usePresenceStore((s) => s.accountIdle);
  const { online, offline } = useMemo(() => visibleRoster(roster, rosterMeta), [roster, rosterMeta]);
  // Only for the rows drawn.
  const statusOf = (m: ServerMember, isOnline: boolean): DisplayStatus => {
    if (isOnline) {
      const s = resolvePresence({ statuses, known, ownStatus, accountIdle }, me, m.uid, true);
      return isOnlineStatus(s) ? s : "online";
    }
    // Ourselves invisible: the invisible dot, as everywhere else.
    return m.uid === me && ownStatus === "invisible" ? "invisible" : "offline";
  };
  const onlineCount = rosterMeta?.onlineCount ?? online.length;
  const offlineCount = rosterMeta ? Math.max(0, rosterMeta.totalMembers - rosterMeta.onlineCount) : offline.length;
  // The end of what's loaded: under the online part until it's complete,
  // under the offline part after.
  const onlineDone = !rosterMeta || rosterMeta.onlineComplete;
  const showOffline = onlineDone && (offline.length > 0 || offlineCount > 0);
  const hasLoadingRow = !!rosterMeta?.hasMore && (!onlineDone || showOffline);

  // Item heights, read off a hidden probe styled like the real items, and
  // again whenever it resizes (the List density setting).
  const probeRef = useRef<HTMLDivElement | null>(null);
  const [metrics, setMetrics] = useState<MemberListMetrics>(DEFAULT_METRICS);
  useLayoutEffect(() => {
    const probe = probeRef.current;
    if (!probe) return;
    const measure = () => {
      const [onlineHeader, offlineHeader, row] = Array.from(probe.children, (c) => (c as HTMLElement).offsetHeight);
      if (!(row > 0)) return;
      setMetrics((cur) =>
        cur.row === row && cur.onlineHeader === onlineHeader && cur.offlineHeader === offlineHeader
          ? cur
          : { row, onlineHeader, offlineHeader },
      );
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(probe);
    return () => ro.disconnect();
  }, []);

  const layout = useMemo(
    () => memberListLayout({ online: online.length, offline: offline.length, showOffline }, metrics),
    [online.length, offline.length, showOffline, metrics],
  );

  // The stretch of the list worth drawing: what the viewport shows plus
  // overscan, snapped so scrolling re-renders once per few rows.
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const [range, setRange] = useState(() => windowRange(0, 1200));
  const latest = useRef({ layout, hasLoadingRow, loadMore });
  latest.current = { layout, hasLoadingRow, loadMore };
  const update = useCallback(() => {
    const scroller = scrollerRef.current;
    const list = listRef.current;
    if (!scroller || !list) return;
    const top = scroller.scrollTop - list.offsetTop;
    const next = windowRange(top, scroller.clientHeight);
    setRange((cur) => (cur.from === next.from && cur.to === next.to ? cur : next));
    // The paging trigger: the end of the loaded stretch nears the bottom.
    const { layout, hasLoadingRow, loadMore } = latest.current;
    if (hasLoadingRow && nearLoadedEnd(layout.total, top, scroller.clientHeight)) loadMore();
  }, []);
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const ro = new ResizeObserver(update);
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [update]);
  // Again after the list changes: a page that didn't fill the viewport
  // pulls the next, and a shorter list may have clamped the scroll.
  useEffect(update, [update, layout, hasLoadingRow, loadMore]);

  const win = memberListWindow(layout, range.from, range.to);

  return (
    <div className="relative flex w-[260px] shrink-0 flex-col border-l border-border bg-bg-dark" style={LIST_VARS}>
      <div ref={probeRef} aria-hidden className="pointer-events-none invisible absolute inset-x-0 top-0">
        <div className={ONLINE_HEADER_CLASS} />
        <div className={OFFLINE_HEADER_CLASS} />
        <div className="h-(--member-row-h)" />
      </div>
      <div ref={scrollerRef} onScroll={update} className="relative flex-1 overflow-y-auto px-3 pt-1 pb-3">
        <div ref={listRef}>
          <div style={{ height: win.before }} />
          {win.items.map((item) => {
            if (item.kind === "header") {
              const isOnline = item.part === "online";
              return (
                <div key={`header-${item.part}`} className={isOnline ? ONLINE_HEADER_CLASS : OFFLINE_HEADER_CLASS}>
                  <h3 className={HEADING_CLASS}>
                    {isOnline ? "Online" : "Offline"} — {(isOnline ? onlineCount : offlineCount).toLocaleString()}
                  </h3>
                </div>
              );
            }
            const isOnline = item.part === "online";
            const m = (isOnline ? online : offline)[item.index];
            return <MemberRow key={m.uid} member={m} status={statusOf(m, isOnline)} serverId={activeServerId} />;
          })}
          <div style={{ height: win.after }} />
        </div>
        {hasLoadingRow && (
          <div className="flex h-11 items-center px-1">
            <button
              type="button"
              onClick={loadMore}
              disabled={rosterMeta!.loadingMore || rosterMeta!.refetching}
              className="h-8 w-full rounded-sm text-[12px] text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary disabled:cursor-default"
            >
              {rosterMeta!.loadingMore || rosterMeta!.refetching ? "Loading…" : "Load more"}
            </button>
          </div>
        )}

        {online.length === 0 && offline.length === 0 && !rosterMeta?.hasMore && (
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
      className="list-row group flex h-(--member-row-h) cursor-pointer items-center rounded-sm transition-colors hover:bg-surface-hover"
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
