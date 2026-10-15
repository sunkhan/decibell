// Self-fetching avatar component used at every avatar render site.
//
// Reads from avatarStore; when the entry is idle (just-invalidated or
// never-seen) it triggers fetchIfNeeded(). Renders <img> when the
// bytes are loaded, LetterAvatar as the fallback (loading / missing /
// error / no-entry-yet states).
//
// Each render site keeps its own ring / status-dot / hover-decoration
// logic on a wrapping container — UserAvatar is only the inner
// "letter-or-image" swap.

import { useEffect } from "react";
import { useAvatarStore } from "../stores/avatarStore";
import { useUserName, type Uid } from "../stores/usersStore";
import { LetterAvatar, AVATAR_RADIUS } from "./LetterAvatar";

interface Props {
  /// The account (avatar image, colour). 0 / negative: no account here
  /// (system sender, a community's legacy placeholder) — letter only.
  uid: Uid;
  /// The name the caller has for them: the letter until the users store
  /// knows the current name.
  name: string;
  size: number;
  className?: string;
}

export function UserAvatar({ uid, name, size, className }: Props) {
  const shown = useUserName(uid, name);
  const entry = useAvatarStore((s) => (uid > 0 ? s.entries.get(uid) : undefined));
  const fetchIfNeeded = useAvatarStore((s) => s.fetchIfNeeded);

  // Re-evaluate every time the entry's status changes; the store
  // only kicks a fetch when status is 'idle' so this is safe to
  // call on every render — it'll no-op once a fetch is in flight
  // or has resolved.
  useEffect(() => {
    if (uid > 0) fetchIfNeeded(uid);
  }, [uid, fetchIfNeeded, entry?.status, entry?.version]);

  if (entry?.status === "loaded" && entry.blobUrl) {
    return (
      <img
        src={entry.blobUrl}
        alt={shown}
        className={`shrink-0 object-cover ${className ?? ""}`}
        style={{
          width: `calc(${size}px * var(--avatar-scale, 1))`,
          height: `calc(${size}px * var(--avatar-scale, 1))`,
          borderRadius: AVATAR_RADIUS,
        }}
        draggable={false}
        // Pre-decoded by the store; sync keeps a remount (rows scrolling
        // back in) from painting a blank frame first.
        decoding="sync"
      />
    );
  }
  return <LetterAvatar uid={uid} name={shown} size={size} className={className} />;
}
