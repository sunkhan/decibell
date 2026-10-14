import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { usePresence } from "../../stores/presenceStore";
import { toast } from "../../stores/toastStore";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import { UserAvatar } from "../../components/UserAvatar";
import { StatusDot } from "../../components/StatusDot";
import { EMPTY_LIST } from "../../lib/empty";
import { nextMemberSearchSeq } from "./tabs/useMemberSearch";
import { inviteFriend } from "./personalInvite";
import type { MemberSearchReceivedPayload, ServerMember } from "../../types";

/// MEMBER_SEARCH_REQ takes at most 200 usernames per lookup.
const LOOKUP_CHUNK = 200;
const NO_ANSWER_MS = 8000;

type RowState = "sending" | "sent";

/// Which of `usernames` are members of the server, asked of the server:
/// the roster store holds only the online members and the first offline
/// page. null until every lookup has answered; a server that doesn't
/// answer falls back to the roster after NO_ANSWER_MS.
function useMembersAmong(
  serverId: string | null,
  usernames: string[],
  active: boolean,
): Set<string> | null {
  const [members, setMembers] = useState<Set<string> | null>(null);
  const key = usernames.join("\n");
  useEffect(() => {
    if (!active || !serverId) return;
    setMembers(null);
    const names = key ? key.split("\n") : [];
    if (names.length === 0) {
      setMembers(new Set());
      return;
    }
    let cancelled = false;
    const pending = new Set<number>();
    const found = new Set<string>();
    const settle = () => {
      if (!cancelled && pending.size === 0) setMembers(new Set(found));
    };
    const un = listen<MemberSearchReceivedPayload>("member_search_received", (e) => {
      const p = e.payload;
      if (p.serverId !== serverId || !pending.delete(p.seq)) return;
      if (p.success) for (const m of p.members) found.add(m.username);
      settle();
    });
    un.then(() => {
      if (cancelled) return;
      for (let i = 0; i < names.length; i += LOOKUP_CHUNK) {
        const seq = nextMemberSearchSeq();
        pending.add(seq);
        invoke("search_members", {
          serverId,
          usernames: names.slice(i, i + LOOKUP_CHUNK),
          limit: LOOKUP_CHUNK,
          seq,
        }).catch(() => {
          pending.delete(seq);
          settle();
        });
      }
    });
    const timer = setTimeout(() => {
      if (!cancelled) setMembers(new Set(found));
    }, NO_ANSWER_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      un.then((fn) => fn());
    };
  }, [active, serverId, key]);
  return members;
}

function FriendRow({
  username,
  state,
  onInvite,
}: {
  username: string;
  state: RowState | undefined;
  onInvite: (username: string) => void;
}) {
  const status = usePresence(username);
  return (
    <div className="flex items-center gap-3 rounded-md px-2 py-1.5">
      <div className="relative shrink-0">
        <UserAvatar username={username} size={32} />
        <StatusDot
          status={status}
          ring="var(--color-bg-dark)"
          className="absolute -bottom-px -right-px h-3 w-3"
        />
      </div>
      <span className="min-w-0 flex-1 truncate font-channel text-[14px] text-text-primary">
        {username}
      </span>
      {state === "sent" ? (
        <span className="flex shrink-0 items-center gap-1.5 px-3 py-1.5 text-[12px] font-medium text-text-muted">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="20 6 9 17 4 12" />
          </svg>
          Sent
        </span>
      ) : (
        <button
          onClick={() => onInvite(username)}
          disabled={state === "sending"}
          className="shrink-0 rounded-sm border border-accent px-3 py-1.5 text-[12px] font-semibold text-accent-bright transition-colors hover:bg-accent hover:text-on-accent disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-accent-bright"
        >
          {state === "sending" ? "Inviting…" : "Invite"}
        </button>
      )}
    </div>
  );
}

/// The welcome banner's "Invite friends": your friends who aren't members
/// yet, each a click away from a personal invite (single-use, never
/// expires, only they can redeem it) sent to them as a DM.
export default function InviteFriendsModal() {
  const open = useUiStore((s) => s.activeModal === "invite-friends");
  const closeModal = useUiStore((s) => s.closeModal);
  useEscapeToClose(closeModal, open);
  const serverId = useChatStore((s) => s.activeServerId);
  const serverName = useChatStore((s) =>
    serverId ? s.servers.find((srv) => srv.id === serverId)?.name : undefined,
  );
  const friends = useFriendsStore((s) => s.friends);
  const roster = useChatStore(
    (s): ServerMember[] => (serverId ? s.membersByServer[serverId] ?? EMPTY_LIST : EMPTY_LIST),
  );
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [search, setSearch] = useState("");
  const [rows, setRows] = useState<Record<string, RowState>>({});

  useEffect(() => {
    if (open) {
      setMounted(true);
      setSearch("");
      setRows({});
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
    }
  }, [open, serverId]);

  // Accepted friends only — not pending requests or blocked users.
  const candidates = useMemo(
    () =>
      friends
        .filter((f) => f.status === "online" || f.status === "offline")
        .map((f) => f.username)
        .sort((a, b) => a.localeCompare(b)),
    [friends],
  );
  const members = useMembersAmong(serverId, candidates, open);
  const notMembers = useMemo(() => {
    if (!members) return [];
    const inRoster = new Set(roster.map((m) => m.username));
    // Someone already invited stays listed (as "Sent") even once they join.
    return candidates.filter((u) => rows[u] === "sent" || (!members.has(u) && !inRoster.has(u)));
  }, [candidates, members, roster, rows]);
  const query = search.trim().toLowerCase();
  const shown = query ? notMembers.filter((u) => u.toLowerCase().includes(query)) : notMembers;

  const handleInvite = useCallback(
    async (username: string) => {
      if (!serverId) return;
      setRows((r) => ({ ...r, [username]: "sending" }));
      try {
        await inviteFriend(serverId, username);
        setRows((r) => ({ ...r, [username]: "sent" }));
      } catch (err) {
        setRows((r) => {
          const next = { ...r };
          delete next[username];
          return next;
        });
        toast.error(`Couldn't invite ${username}`, err instanceof Error ? err.message : String(err));
      }
    },
    [serverId],
  );

  if (!mounted || !serverId) return null;

  let empty: string | null = null;
  if (!members) empty = null;
  else if (candidates.length === 0) empty = "Add some friends first — they'll show up here.";
  else if (notMembers.length === 0) empty = "All your friends are already here.";
  else if (shown.length === 0) empty = "No friends match that search.";

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={closeModal}
      onTransitionEnd={(e) => {
        if (e.target === e.currentTarget && !visible) setMounted(false);
      }}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-[440px] flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal transition-[opacity,transform] duration-300"
        style={{ opacity: visible ? 1 : 0, transform: visible ? "scale(1)" : "scale(0.95)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-start justify-between gap-3 border-b border-border-divider px-6 py-5">
          <div className="min-w-0">
            <h2 className="truncate font-display text-[18px] font-semibold text-text-primary">
              Invite friends to {serverName ?? "this server"}
            </h2>
            <p className="mt-0.5 text-[12px] text-text-muted">
              Each friend gets their own invite in a DM. It works once, only for them, and never expires.
            </p>
          </div>
          <button
            onClick={closeModal}
            className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div className="shrink-0 px-6 pt-4">
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search friends"
            spellCheck={false}
            autoFocus
            className="w-full rounded-md border border-border bg-bg-light px-3 py-2 text-[13px] text-text-primary outline-none transition-all placeholder:text-text-muted focus:border-accent focus:shadow-ring"
          />
        </div>

        {/* A fixed height (six rows, then it scrolls), so the card doesn't
            jump when the lookup answers or a search empties it; it can
            still shrink on a short window. */}
        <div className="h-72 min-h-0 overflow-y-auto px-4 py-3 scrollbar-thin">
          {!members ? (
            <div className="flex h-full items-center justify-center py-8 text-[13px] text-text-muted">
              Checking who's already here…
            </div>
          ) : empty ? (
            <div className="flex h-full items-center justify-center py-8 text-center text-[13px] text-text-muted">
              {empty}
            </div>
          ) : (
            shown.map((u) => (
              <FriendRow key={u} username={u} state={rows[u]} onInvite={handleInvite} />
            ))
          )}
        </div>
      </div>
    </div>
  );
}
