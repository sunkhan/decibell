// Letter-in-gradient fallback. Lifted from the existing inline markup
// scattered across UserPanel / ConversationSidebar / MembersList /
// MessageBubble etc. Used standalone for cases where letter avatars
// still make sense, and as the fallback for UserAvatar (loading /
// missing / error states).

import { userGradient } from "../utils/colors";

/// Avatar corner radius, per theme. `nocturne`, `matinee` and
/// `graphite*` keep the DS v1
/// squircle — a constant 26% radius-to-size ratio, so a 22px voice
/// participant reads as the same shape instead of collapsing into a
/// circle the way a fixed radius does at small sizes. `console*`
/// override it to their flat 4px, which is why this is a CSS variable
/// and no longer a size × ratio computed here: a ratio can't express
/// "the same sharp corner at every size".
///
/// Every ring drawn around an avatar (the profile popup's 4px border,
/// status-dot cutouts) assumes this value, so keep them concentric if
/// you change it.
export const AVATAR_RADIUS = "var(--radius-avatar)";

interface Props {
  /// Colours hash the account uid (stable across renames); without one
  /// (0 / negative) they hash the name.
  uid: number;
  name: string;
  size: number;
  className?: string;
}

export function LetterAvatar({ uid, name, size, className }: Props) {
  const initial = (name.charAt(0) || "?").toUpperCase();
  return (
    <div
      className={`flex shrink-0 items-center justify-center font-semibold ${className ?? ""}`}
      style={{
        width: `calc(${size}px * var(--avatar-scale, 1))`,
        height: `calc(${size}px * var(--avatar-scale, 1))`,
        borderRadius: AVATAR_RADIUS,
        // Per-theme initial colour: the light palettes keep white on
        // their darker fills, but the token is the contract, not white.
        color: "var(--color-av-fg)",
        // Same gradient generator the existing inline avatars use, so
        // a user's letter colour stays consistent across rendering
        // sites and across the load → fail-back transition.
        background: userGradient(uid, name),
        fontSize: `calc(${Math.max(10, Math.floor(size * 0.42))}px * var(--avatar-scale, 1))`,
      }}
    >
      {initial}
    </div>
  );
}
