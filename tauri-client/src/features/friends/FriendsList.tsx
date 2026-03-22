import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useFriendsStore } from "../../stores/friendsStore";
import FriendActionButton from "./FriendActionButton";
import type { FriendInfo } from "../../types";
import { stringToColor } from "../../utils/colors";

function FriendRow({ friend }: { friend: FriendInfo }) {
  const isOnline = friend.status === "online";
  const isPendingIn = friend.status === "pending_incoming";
  const isPendingOut = friend.status === "pending_outgoing";

  return (
    <div className="flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-white/5">
      {/* Avatar with status dot */}
      <div className="relative flex-shrink-0">
        <div
          className="flex h-8 w-8 items-center justify-center rounded-lg text-xs font-bold text-white"
          style={{ backgroundColor: stringToColor(friend.username) }}
        >
          {friend.username.charAt(0).toUpperCase()}
        </div>
        {(friend.status === "online" || friend.status === "offline") && (
          <div
            className={`absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-bg-primary ${
              isOnline ? "bg-success" : "bg-[#4f6a86]"
            }`}
          />
        )}
      </div>

      {/* Username */}
      <span className="min-w-0 flex-1 truncate text-sm text-text-primary">
        {friend.username}
      </span>

      {/* Actions */}
      {isPendingIn && (
        <div className="flex gap-1">
          <FriendActionButton
            action="ACCEPT"
            targetUsername={friend.username}
            label="Accept"
            variant="success"
          />
          <FriendActionButton
            action="REJECT"
            targetUsername={friend.username}
            label="Reject"
            variant="error"
          />
        </div>
      )}
      {isPendingOut && (
        <span className="text-xs text-text-muted">Pending</span>
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

  const filtered = friends.filter((f) =>
    f.username.toLowerCase().includes(search.toLowerCase())
  );

  const sections: { label: string; items: FriendInfo[] }[] = [
    {
      label: "ONLINE",
      items: filtered.filter((f) => f.status === "online"),
    },
    {
      label: "OFFLINE",
      items: filtered.filter((f) => f.status === "offline"),
    },
    {
      label: "PENDING",
      items: filtered.filter(
        (f) =>
          f.status === "pending_incoming" || f.status === "pending_outgoing"
      ),
    },
    {
      label: "BLOCKED",
      items: filtered.filter((f) => f.status === "blocked"),
    },
  ];

  const handleAddFriend = async () => {
    if (!addUsername.trim()) return;
    setAddError(null);
    useFriendsStore.getState().setLastActionError(null);
    try {
      await invoke("send_friend_action", {
        action: 0, // ADD
        targetUsername: addUsername.trim(),
      });
      setAddUsername("");
      setShowAdd(false);
    } catch (err) {
      setAddError(String(err));
    }
  };

  return (
    <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 className="text-sm font-semibold text-text-primary">Friends</h2>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="text-xs text-accent hover:underline"
        >
          Add Friend
        </button>
      </div>

      {/* Add friend input */}
      {showAdd && (
        <>
          <div className="flex gap-2 border-b border-border px-3 py-2">
            <input
              type="text"
              value={addUsername}
              onChange={(e) => setAddUsername(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAddFriend()}
              placeholder="Username"
              className="flex-1 rounded-md border border-border bg-bg-primary px-2 py-1 text-sm text-text-primary outline-none focus:border-accent"
            />
            <button
              onClick={handleAddFriend}
              className="rounded-md bg-accent px-2 py-1 text-xs font-semibold text-white hover:bg-accent-hover"
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
          className="w-full rounded-md border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-text-primary outline-none placeholder:text-text-muted focus:border-accent"
        />
      </div>

      {lastActionError && (
        <div className="mx-3 mb-1 rounded-md bg-error/10 px-2.5 py-1.5 text-xs text-error">
          {lastActionError}
        </div>
      )}

      {/* Friend sections */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {sections.map(
          (section) =>
            section.items.length > 0 && (
              <div key={section.label} className="mb-3">
                <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
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
