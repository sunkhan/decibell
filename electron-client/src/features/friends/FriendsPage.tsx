// Full-width Friends surface for the home view.
//
// FriendsList is the 260px rail that sits beside a DM conversation.
// This is the same data given the whole area right of the DM list, so
// search gets real width, Add Friend becomes a proper button, and each
// row has somewhere to put its actions. Both surfaces stay: the rail is
// still the right shape next to an open conversation.
//
// Design: design_handoff_theme_switcher-era proposal, reviewed 2026-07-26.

import { useEffect, useMemo, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useFriendsStore } from "../../stores/friendsStore";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { UserAvatar } from "../../components/UserAvatar";
import type { FriendInfo } from "../../types";

// Mirrors FriendActionType in proto/messages.proto.
const ACTION = { ADD: 0, REMOVE: 1, BLOCK: 2, ACCEPT: 3, REJECT: 4 } as const;

type Filter = "all" | "online" | "pending" | "blocked";

/// Pending leads: an unanswered request is the only thing on this
/// screen that needs a decision, so it shouldn't sit below a long
/// offline list.
const SECTIONS: { key: string; label: string; statuses: FriendInfo["status"][] }[] = [
  { key: "pending", label: "Pending", statuses: ["pending_incoming", "pending_outgoing"] },
  { key: "online", label: "Online", statuses: ["online"] },
  { key: "offline", label: "Offline", statuses: ["offline"] },
  { key: "blocked", label: "Blocked", statuses: ["blocked"] },
];

const FILTER_STATUSES: Record<Filter, FriendInfo["status"][] | null> = {
  all: null,
  online: ["online"],
  pending: ["pending_incoming", "pending_outgoing"],
  blocked: ["blocked"],
};

const STATUS_LABEL: Record<FriendInfo["status"], string> = {
  online: "Online",
  offline: "Offline",
  pending_incoming: "Wants to be friends",
  pending_outgoing: "Request sent",
  blocked: "Blocked",
};

function Icon({ d, size = 16 }: { d: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className="shrink-0"
      dangerouslySetInnerHTML={{ __html: d }}
    />
  );
}

const PATH = {
  search: '<path d="M11 19a8 8 0 100-16 8 8 0 000 16zM21 21l-4.35-4.35"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  message:
    '<path d="M21 11.5a8.38 8.38 0 01-.9 3.8 8.5 8.5 0 01-7.6 4.7 8.38 8.38 0 01-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 01-.9-3.8 8.5 8.5 0 014.7-7.6 8.38 8.38 0 013.8-.9h.5a8.48 8.48 0 018 8v.5z"/>',
  more:
    '<circle cx="12" cy="12" r="1.6"/><circle cx="19" cy="12" r="1.6"/><circle cx="5" cy="12" r="1.6"/>',
  check: '<path d="M20 6L9 17l-5-5"/>',
  x: '<path d="M18 6L6 18M6 6l12 12"/>',
};

/// Shared shape for the row's trailing controls. Filled for the
/// affirmative action, outlined for everything else — the outlined
/// variant turns red on hover only where the action is destructive.
function RowButton({
  label,
  iconPath,
  variant,
  onClick,
}: {
  label: string;
  iconPath?: string;
  variant: "filled" | "outline" | "danger";
  onClick: () => void;
}) {
  const look = {
    filled: "bg-accent text-on-accent hover:bg-accent-hover",
    outline:
      "border border-border bg-bg-light text-text-secondary hover:text-text-primary",
    danger:
      "border border-border bg-bg-light text-text-secondary hover:border-error hover:text-error",
  }[variant];
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      title={label}
      className={`flex h-[30px] shrink-0 items-center gap-1.5 rounded-md px-2.5 font-channel text-member font-semibold transition-colors focus-visible:shadow-ring focus-visible:outline-none ${look}`}
    >
      {iconPath && <Icon d={iconPath} size={15} />}
      {label}
    </button>
  );
}

/// Receives the event so a button can anchor a popover to its own
/// rect — `More` opens the shared user menu beside itself.
function IconButton({
  label,
  iconPath,
  onClick,
}: {
  label: string;
  iconPath: string;
  onClick: (e: React.MouseEvent) => void;
}) {
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick(e);
      }}
      title={label}
      aria-label={label}
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border-divider bg-bg-light text-text-muted transition-colors hover:bg-bg-lighter hover:text-text-primary focus-visible:shadow-ring focus-visible:outline-none"
    >
      <Icon d={iconPath} />
    </button>
  );
}

function FriendRow({ friend }: { friend: FriendInfo }) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const { username, status } = friend;

  const act = (action: number) => {
    invoke("send_friend_action", { action, targetUsername: username }).catch(
      (e) => useFriendsStore.getState().setLastActionError(String(e)),
    );
  };

  const openMessage = () => {
    setActiveDmUser(username);
    setActiveView("dm");
  };

  // The user menu is 220px wide; right-align it under the button and
  // keep it on screen on a narrow window.
  const openMenuFromButton = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openContextMenu(username, { x: Math.max(8, r.right - 220), y: r.bottom + 6 });
  };

  // Only an accepted friend has a conversation to open.
  const canMessage = status === "online" || status === "offline";

  let actions: React.ReactNode;
  // Requests keep their buttons visible: something awaiting a decision
  // shouldn't be hidden behind a hover.
  let pinned = false;
  if (status === "pending_incoming") {
    pinned = true;
    actions = (
      <>
        <RowButton label="Accept" iconPath={PATH.check} variant="filled" onClick={() => act(ACTION.ACCEPT)} />
        <RowButton label="Decline" iconPath={PATH.x} variant="danger" onClick={() => act(ACTION.REJECT)} />
      </>
    );
  } else if (status === "pending_outgoing") {
    pinned = true;
    actions = (
      <RowButton label="Cancel" iconPath={PATH.x} variant="danger" onClick={() => act(ACTION.REMOVE)} />
    );
  } else if (status === "blocked") {
    actions = (
      <RowButton label="Unblock" variant="outline" onClick={() => act(ACTION.REMOVE)} />
    );
  } else {
    actions = (
      <>
        <IconButton label="Message" iconPath={PATH.message} onClick={openMessage} />
        <IconButton label="More" iconPath={PATH.more} onClick={openMenuFromButton} />
      </>
    );
  }

  return (
    <div
      className="list-row group flex cursor-pointer items-center rounded-md transition-colors hover:bg-surface-hover focus-visible:shadow-ring focus-visible:outline-none"
      tabIndex={0}
      onClick={(e) => {
        // A friend row is a shortcut into the conversation — same as the
        // Message button, which stays as the visible affordance. Rows
        // you can't message (a request either way, or someone blocked)
        // fall back to the profile popup, since there's nothing to open.
        if (canMessage) {
          openMessage();
          return;
        }
        const r = e.currentTarget.getBoundingClientRect();
        openProfilePopup(username, { x: r.left + 40, y: r.top });
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY });
      }}
    >
      <div className="relative shrink-0">
        <UserAvatar username={username} size={36} />
        {(status === "online" || status === "offline") && (
          <div
            className={`avatar-dot absolute -bottom-px -right-px rounded-full border-[2.5px] border-bg-mid ${
              status === "online" ? "bg-success" : "bg-text-muted"
            }`}
          />
        )}
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate font-channel text-body font-medium text-text-primary">
          {username}
        </div>
        <div className="font-mono text-micro text-text-muted">{STATUS_LABEL[status]}</div>
      </div>
      <div
        className={`flex shrink-0 items-center gap-1.5 transition-opacity ${
          pinned ? "" : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100"
        }`}
      >
        {actions}
      </div>
    </div>
  );
}

function Segment({
  label,
  count,
  alert,
  active,
  onClick,
}: {
  label: string;
  count?: number;
  alert?: boolean;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-sm px-2.5 py-[5px] font-channel text-member font-medium transition-colors focus-visible:shadow-ring focus-visible:outline-none ${
        active
          ? "bg-accent-soft text-accent-bright"
          : "text-text-muted hover:bg-surface-hover hover:text-text-primary"
      }`}
    >
      {label}
      {count !== undefined && count > 0 && (
        <span
          className={`rounded-sm px-1.5 font-mono text-micro tabular-nums ${
            alert
              ? "bg-error text-white"
              : active
                ? "bg-accent-mid text-accent-bright"
                : "bg-surface-active text-text-muted"
          }`}
        >
          {count}
        </span>
      )}
    </button>
  );
}

export default function FriendsPage() {
  const friends = useFriendsStore((s) => s.friends);
  const lastActionError = useFriendsStore((s) => s.lastActionError);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [addOpen, setAddOpen] = useState(false);
  const [addUsername, setAddUsername] = useState("");
  const [addError, setAddError] = useState<string | null>(null);

  useEffect(() => {
    if (!addError) return;
    const t = setTimeout(() => setAddError(null), 5000);
    return () => clearTimeout(t);
  }, [addError]);

  useEffect(() => {
    if (!lastActionError) return;
    const t = setTimeout(() => useFriendsStore.getState().setLastActionError(null), 5000);
    return () => clearTimeout(t);
  }, [lastActionError]);

  const counts = useMemo(() => {
    const by = (statuses: FriendInfo["status"][]) =>
      friends.filter((f) => statuses.includes(f.status)).length;
    return {
      accepted: by(["online", "offline"]),
      online: by(["online"]),
      // Only incoming requests are a call to action, so the badge
      // counts those rather than the whole pending group.
      pendingIn: by(["pending_incoming"]),
      pending: by(["pending_incoming", "pending_outgoing"]),
      blocked: by(["blocked"]),
    };
  }, [friends]);

  const sections = useMemo(() => {
    const needle = search.trim().toLowerCase();
    const allowed = FILTER_STATUSES[filter];
    return SECTIONS.map((s) => ({
      ...s,
      items: friends.filter(
        (f) =>
          s.statuses.includes(f.status) &&
          (allowed === null || allowed.includes(f.status)) &&
          f.username.toLowerCase().includes(needle),
      ),
    })).filter((s) => s.items.length > 0);
  }, [friends, search, filter]);

  const shown = sections.reduce((n, s) => n + s.items.length, 0);

  const handleAdd = async () => {
    const target = addUsername.trim();
    if (!target) return;
    setAddError(null);
    useFriendsStore.getState().setLastActionError(null);
    try {
      await invoke("send_friend_action", { action: ACTION.ADD, targetUsername: target });
      setAddUsername("");
      setAddOpen(false);
    } catch (err) {
      setAddError(String(err));
    }
  };

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      <div className="flex h-12 shrink-0 items-center gap-3 border-b border-border-divider px-5">
        <h2 className="font-display text-title font-emphasis tracking-title text-text-bright">
          Friends
        </h2>
        <span className="font-mono text-micro tabular-nums text-text-muted">
          {counts.accepted}
        </span>
        <div className="ml-auto flex items-center gap-0.5" role="tablist" aria-label="Filter friends">
          <Segment label="All" active={filter === "all"} onClick={() => setFilter("all")} />
          <Segment
            label="Online"
            count={counts.online}
            active={filter === "online"}
            onClick={() => setFilter("online")}
          />
          <Segment
            label="Pending"
            count={counts.pendingIn || counts.pending}
            alert={counts.pendingIn > 0}
            active={filter === "pending"}
            onClick={() => setFilter("pending")}
          />
          <Segment
            label="Blocked"
            count={counts.blocked}
            active={filter === "blocked"}
            onClick={() => setFilter("blocked")}
          />
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-2.5 border-b border-border-divider px-5 py-3">
        <div className="relative min-w-0 flex-1">
          <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-text-muted">
            <Icon d={PATH.search} size={15} />
          </span>
          <input
            type="search"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search friends"
            aria-label="Search friends"
            className="h-9 w-full rounded-md border border-border bg-bg-input pl-[34px] pr-3 font-channel text-member text-text-primary outline-none transition-all placeholder:text-text-muted focus:border-accent focus:shadow-ring"
          />
        </div>
        <button
          type="button"
          onClick={() => setAddOpen((v) => !v)}
          aria-expanded={addOpen}
          className="flex h-9 shrink-0 items-center gap-1.5 rounded-md bg-accent px-4 font-channel text-member font-semibold text-on-accent transition-colors hover:bg-accent-hover focus-visible:shadow-ring focus-visible:outline-none"
        >
          <Icon d={PATH.plus} />
          Add Friend
        </button>
      </div>

      {addOpen && (
        <div className="shrink-0 animate-[dropIn_0.16s_ease_both] border-b border-border-divider bg-bg-light px-5 py-3.5">
          <label
            htmlFor="add-friend-username"
            className="mb-1.5 block font-mono text-micro uppercase tracking-[0.1em] text-text-muted"
          >
            Send a friend request
          </label>
          <div className="flex gap-2">
            <input
              id="add-friend-username"
              type="text"
              autoFocus
              value={addUsername}
              onChange={(e) => setAddUsername(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAdd()}
              placeholder="Username"
              className="h-[34px] min-w-0 flex-1 rounded-md border border-border bg-bg-input px-3 font-channel text-member text-text-primary outline-none transition-all placeholder:text-text-muted focus:border-accent focus:shadow-ring"
            />
            <button
              type="button"
              onClick={handleAdd}
              className="h-[34px] shrink-0 rounded-md bg-accent px-3.5 font-channel text-member font-semibold text-on-accent transition-colors hover:bg-accent-hover"
            >
              Send request
            </button>
          </div>
          <p className="mt-1.5 font-mono text-micro text-text-muted">
            Usernames are case-sensitive.
          </p>
          {addError && <p className="mt-1.5 text-micro text-error">{addError}</p>}
        </div>
      )}

      {lastActionError && (
        <div className="mx-5 mt-2 shrink-0 rounded-md bg-error/10 px-3 py-2 text-member text-error">
          {lastActionError}
        </div>
      )}

      <div
        className="flex-1 overflow-y-auto px-3 pb-5 pt-1"
        style={
          {
            "--list-row-pad-y": "8px",
            "--list-row-pad-x": "8px",
            "--list-row-gap": "12px",
          } as React.CSSProperties
        }
      >
        {sections.map((section) => (
          <div key={section.key} className="mt-2.5">
            {/* With one status selected the segment above already says
                what you're looking at, so the header is noise. */}
            {filter === "all" && (
              <h3 className="mb-1 flex items-center gap-2 px-2 font-mono text-section font-medium uppercase leading-none tracking-section text-text-muted">
                {section.label}
                <span className="tabular-nums opacity-75">{section.items.length}</span>
              </h3>
            )}
            {section.items.map((f) => (
              <FriendRow key={f.username} friend={f} />
            ))}
          </div>
        ))}

        {shown === 0 && (
          <div className="flex flex-col items-center gap-1.5 pt-14 text-center">
            <strong className="font-channel text-body font-medium text-text-secondary">
              {friends.length === 0 ? "No friends yet" : "Nothing here"}
            </strong>
            <span className="font-mono text-micro text-text-muted">
              {friends.length === 0
                ? "Add someone with the button above"
                : "No friends match this filter"}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
