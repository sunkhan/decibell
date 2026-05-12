// Letter-in-gradient fallback. Lifted from the existing inline markup
// scattered across UserPanel / ConversationSidebar / MembersList /
// MessageBubble etc. Used standalone for cases where letter avatars
// still make sense, and as the fallback for UserAvatar (loading /
// missing / error states).

import { stringToGradient } from "../utils/colors";

interface Props {
  username: string;
  size: number;
  className?: string;
}

export function LetterAvatar({ username, size, className }: Props) {
  const initial = (username.charAt(0) || "?").toUpperCase();
  return (
    <div
      className={`flex shrink-0 items-center justify-center rounded-md font-semibold text-white ${className ?? ""}`}
      style={{
        width: size,
        height: size,
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
