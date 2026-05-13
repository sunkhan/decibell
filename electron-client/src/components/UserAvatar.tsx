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
import { LetterAvatar } from "./LetterAvatar";

interface Props {
  username: string;
  size: number;
  className?: string;
}

export function UserAvatar({ username, size, className }: Props) {
  const entry = useAvatarStore((s) => s.entries.get(username));
  const fetchIfNeeded = useAvatarStore((s) => s.fetchIfNeeded);

  // Re-evaluate every time the entry's status changes; the store
  // only kicks a fetch when status is 'idle' so this is safe to
  // call on every render — it'll no-op once a fetch is in flight
  // or has resolved.
  useEffect(() => {
    fetchIfNeeded(username);
  }, [username, fetchIfNeeded, entry?.status, entry?.version]);

  if (entry?.status === "loaded" && entry.blobUrl) {
    return (
      <img
        src={entry.blobUrl}
        alt={username}
        className={`shrink-0 rounded-md object-cover ${className ?? ""}`}
        style={{ width: size, height: size }}
        draggable={false}
      />
    );
  }
  return <LetterAvatar username={username} size={size} className={className} />;
}
