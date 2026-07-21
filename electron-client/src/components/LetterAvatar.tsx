// Letter-in-gradient fallback. Lifted from the existing inline markup
// scattered across UserPanel / ConversationSidebar / MembersList /
// MessageBubble etc. Used standalone for cases where letter avatars
// still make sense, and as the fallback for UserAvatar (loading /
// missing / error states).

import { stringToGradient } from "../utils/colors";

/// DS v1 squircle: avatars keep a constant radius-to-size ratio
/// (10px on the flagship 38px message avatar ≈ 0.26) so a 22px voice
/// participant reads as the same shape instead of collapsing into a
/// circle the way a fixed 10px radius does at small sizes.
export function avatarRadius(size: number): number {
  return Math.round(size * 0.26);
}

interface Props {
  username: string;
  size: number;
  className?: string;
}

export function LetterAvatar({ username, size, className }: Props) {
  const initial = (username.charAt(0) || "?").toUpperCase();
  return (
    <div
      className={`flex shrink-0 items-center justify-center font-semibold text-white ${className ?? ""}`}
      style={{
        width: size,
        height: size,
        borderRadius: avatarRadius(size),
        // Same gradient generator the existing inline avatars use, so
        // a user's letter colour stays consistent across rendering
        // sites and across the load → fail-back transition.
        background: stringToGradient(username),
        fontSize: Math.max(10, Math.floor(size * 0.42)),
      }}
    >
      {initial}
    </div>
  );
}
