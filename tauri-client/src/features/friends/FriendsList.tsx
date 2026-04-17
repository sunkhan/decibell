import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useFriendsStore } from "../../stores/friendsStore";
import { useUiStore } from "../../stores/uiStore";
import FriendActionButton from "./FriendActionButton";
import type { FriendInfo } from "../../types";
import { stringToGradient } from "../../utils/colors";

function FriendRow({ friend }: { friend: FriendInfo }) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const isOnline = friend.status === "online";
  const isPendingIn = friend.status === "pending_incoming";
  const isPendingOut = friend.status === "pending_outgoing";

  return (
    <div
      className="group flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(friend.username, { x: rect.right + 8, y: rect.top });
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(friend.username, { x: e.clientX, y: e.clientY });
      }}
    >
      <div className="relative shrink-0">
        <div
          className="flex h-8 w-8 items-center justify-center rounded-lg text-[13px] font-bold text-white"
          style={{ background: stringToGradient(friend.username) }}
        >
          {friend.username.charAt(0).toUpperCase()}
        </div>
        {(friend.status === "online" || friend.status === "offline") && (
          <div
            className={`absolute -bottom-px -right-px h-[10px] w-[10px] rounded-full border-[2.5px] border-bg-secondary ${
              isOnline ? "bg-success" : "bg-text-muted"
            }`}
          />
        )}
      </div>
      <span className="min-w-0 flex-1 truncate font-channel text-[13px] font-normal text-text-secondary transition-colors group-hover:text-text-primary">
        {friend.username}
      </span>
      {isPendingIn && (
        <div className="flex gap-1">
          <FriendActionButton action="ACCEPT" targetUsername={friend.username} label="Accept" variant="success" />
          <FriendActionButton action="REJECT" targetUsername={friend.username} label="Reject" variant="error" />
        </div>
      )}
      {isPendingOut && (
        <span className="text-[11px] text-text-muted">Pending</span>
      )}
    </div>
  );
}

export default function FriendsList() {
  const friends = useFriendsStore((s) => s.friends);
  const lastActionError = useFriendsStore((s) => s.lastActionError);
  const [search, setSearch] = useState("");
  const [addUsername, setAddUsername] = useState("");
  const [showAdd, setShowAdd] = useState(false);
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

  const filtered = friends.filter((f) =>
    f.username.toLowerCase().includes(search.toLowerCase())
  );

  const sections: { label: string; items: FriendInfo[] }[] = [
    { label: "ONLINE", items: filtered.filter((f) => f.status === "online") },
    { label: "OFFLINE", items: filtered.filter((f) => f.status === "offline") },
    { label: "PENDING", items: filtered.filter((f) => f.status === "pending_incoming" || f.status === "pending_outgoing") },
    { label: "BLOCKED", items: filtered.filter((f) => f.status === "blocked") },
  ];

  const handleAddFriend = async () => {
    if (!addUsername.trim()) return;
    setAddError(null);
    useFriendsStore.getState().setLastActionError(null);
    try {
      await invoke("send_friend_action", {
        action: 0,
        targetUsername: addUsername.trim(),
      });
      setAddUsername("");
      setShowAdd(false);
    } catch (err) {
      setAddError(String(err));
    }
  };

  return (
    <div className="flex w-[260px] shrink-0 flex-col border-l border-border bg-bg-dark">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 className="font-display text-[15px] font-semibold text-text-bright">Friends</h2>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="rounded-md px-2 py-1 font-channel text-[12px] font-medium text-accent-bright transition-colors hover:bg-accent-hover/15"
        >
          Add Friend
        </button>
      </div>

      {/* Add friend input */}
      {showAdd && (
        <>
          <div className="flex gap-1.5 border-b border-border px-3 py-2.5">
            <input
              type="text"
              value={addUsername}
              onChange={(e) => setAddUsername(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAddFriend()}
              placeholder="Username"
              className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-text-primary outline-none transition-colors focus:border-accent"
            />
            <button
              onClick={handleAddFriend}
              className="shrink-0 rounded-lg bg-accent px-2.5 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-accent-hover"
            >
              Send
            </button>
          </div>
          {addError && (
            <p className="px-3 pb-1 text-xs text-error">{addError}</p>
          )}
        </>
      )}

      {/* Search */}
      <div className="px-3 py-2">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search friends..."
          className="w-full rounded-[10px] border border-border bg-bg-light px-3 py-2 text-[12px] text-text-primary outline-none transition-all focus:border-accent focus:shadow-[0_0_0_2px_var(--color-accent-soft)] placeholder:font-channel placeholder:text-[12px] placeholder:font-normal placeholder:text-text-faint"
        />
      </div>

      {lastActionError && (
        <div className="mx-3 mb-1 rounded-lg bg-error/10 px-2.5 py-1.5 text-xs text-error">
          {lastActionError}
        </div>
      )}

      {/* Friend sections */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {sections.map(
          (section) =>
            section.items.length > 0 && (
              <div key={section.label} className="mb-3">
                <h3 className="mb-1 px-2 font-channel text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                  {section.label} — {section.items.length}
                </h3>
                {section.items.map((friend) => (
                  <FriendRow key={friend.username} friend={friend} />
                ))}
              </div>
            )
        )}
        {friends.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No friends yet. Add someone!
          </p>
        )}
        {friends.length > 0 && filtered.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No results found
          </p>
        )}
      </div>
    </div>
  );
}
