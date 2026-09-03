import { useE2eeStore } from "../../stores/e2eeStore";
import { LockGlyph } from "../chat/MessageBubble";
import { usePeerE2ee } from "./usePeerE2ee";

/// DM-header pill next to the Online/Offline badge: whether messages in
/// this conversation are sealed right now, and why not when they aren't.
/// Renders nothing on a central without the key endpoints, and nothing
/// while the peer's state is still being looked up (no "Unencrypted"
/// flash on open).
export default function EncryptionBadge({ peer }: { peer: string }) {
  const status = useE2eeStore((s) => s.status);
  const info = usePeerE2ee(peer);
  if (status === "unavailable") return null;
  if (status === "ready" && !info) return null;

  const on = status === "ready" && !!info?.hasKeys;
  const title = on
    ? "End-to-end encrypted. Only you and this person can read these messages"
    : status === "locked"
      ? "Encryption is locked on this device. Enter your passphrase in Settings → Privacy"
      : status === "not_set_up"
        ? "Set up end-to-end encryption in Settings → Privacy"
        : `${peer} hasn't set up encryption yet, messages are not encrypted`;

  return (
    <div
      title={title}
      className={`flex items-center gap-[5px] rounded-sm px-2 py-0.5 font-channel text-[11px] font-medium ${
        on ? "bg-success/15 text-success" : "bg-text-muted/15 text-text-muted"
      }`}
    >
      {on ? (
        <LockGlyph size={11} />
      ) : (
        <svg
          width="11"
          height="11"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <rect x="3" y="11" width="18" height="11" rx="2" />
          <path d="M7 11V7a5 5 0 019.9-1" />
        </svg>
      )}
      {on ? "Encrypted" : "Unencrypted"}
    </div>
  );
}
