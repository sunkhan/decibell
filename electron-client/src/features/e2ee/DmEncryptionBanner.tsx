import { useE2eeStore } from "../../stores/e2eeStore";
import { LockGlyph } from "../chat/MessageBubble";
import { usePeerE2ee } from "./usePeerE2ee";

/// One slim strip under the DM header, for the states that need the
/// user's attention (in priority order): this device is locked; the peer's
/// keys changed; encryption isn't set up yet (dismissable nudge); the peer
/// has no keys. Nothing when everything is sealed or the central can't do
/// encryption.
export default function DmEncryptionBanner({ peer }: { peer: string }) {
  const status = useE2eeStore((s) => s.status);
  const nudgeDismissed = useE2eeStore((s) => s.setupNudgeDismissed);
  const changedAt = useE2eeStore((s) => s.changedPeers[peer] ?? 0);
  const openModal = useE2eeStore((s) => s.openPassphraseModal);
  const dismissNudge = useE2eeStore((s) => s.dismissSetupNudge);
  const dismissChange = useE2eeStore((s) => s.dismissPeerChange);
  const info = usePeerE2ee(peer);

  if (status === "unavailable") return null;

  let body: React.ReactNode = null;
  let action: React.ReactNode = null;
  let tone: "warning" | "info" | "muted" = "info";

  if (status === "locked") {
    tone = "warning";
    body = "Encryption is locked on this device. Enter your passphrase to read and send encrypted messages.";
    action = (
      <ActionButton onClick={() => openModal("unlock")} primary>
        Unlock
      </ActionButton>
    );
  } else if (changedAt > 0) {
    tone = "warning";
    body = `${peer}'s encryption keys changed. If that's unexpected, compare safety numbers in their profile before trusting new messages.`;
    action = <ActionButton onClick={() => dismissChange(peer)}>Dismiss</ActionButton>;
  } else if (status === "not_set_up") {
    if (nudgeDismissed) return null;
    body = `Set up end-to-end encryption so only you and ${peer} can read your messages — not even the server.`;
    action = (
      <>
        <ActionButton onClick={dismissNudge}>Not now</ActionButton>
        <ActionButton onClick={() => openModal("setup")} primary>
          Set up
        </ActionButton>
      </>
    );
  } else if (status === "ready" && info && !info.hasKeys) {
    tone = "muted";
    body = `Messages with ${peer} aren't encrypted yet — they haven't set up encryption.`;
  } else {
    return null;
  }

  const toneClass =
    tone === "warning"
      ? "bg-warning/10 text-text-primary"
      : tone === "info"
        ? "bg-accent-soft text-text-primary"
        : "bg-bg-light text-text-muted";
  const iconClass = tone === "warning" ? "text-warning" : tone === "info" ? "text-accent" : "text-text-muted";

  return (
    <div className={`flex shrink-0 items-center gap-3 border-b border-border px-4 py-2 text-[12px] leading-[1.45] ${toneClass}`}>
      <span className={`shrink-0 ${iconClass}`}>
        <LockGlyph size={14} />
      </span>
      <span className="min-w-0 flex-1">{body}</span>
      {action && <span className="flex shrink-0 items-center gap-1.5">{action}</span>}
    </div>
  );
}

function ActionButton({
  children,
  onClick,
  primary = false,
}: {
  children: React.ReactNode;
  onClick: () => void;
  primary?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={
        primary
          ? "rounded-sm bg-accent px-3 py-1 text-[12px] font-semibold text-on-accent hover:bg-accent-hover"
          : "rounded-sm border border-border px-3 py-1 text-[12px] font-medium text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
      }
    >
      {children}
    </button>
  );
}
