import { useE2eeStore } from "../../stores/e2eeStore";
import { LockGlyph } from "../chat/MessageBubble";

/// Privacy-tab card for end-to-end encrypted DMs. State-driven: the
/// central's support, then whether this account has keys and whether this
/// device holds them. All actions go through PassphraseModal.
export default function E2eeSection() {
  const supported = useE2eeStore((s) => s.supported);
  const status = useE2eeStore((s) => s.status);
  const fingerprint = useE2eeStore((s) => s.fingerprint);
  const openModal = useE2eeStore((s) => s.openPassphraseModal);

  let description: string;
  let badge: { text: string; on: boolean } | null = null;
  let actions: React.ReactNode = null;

  if (!supported || status === "unavailable") {
    description =
      "Encrypts your direct messages on your device so only you and the person you're talking to can read them. Your central server doesn't offer this yet.";
  } else if (status === "not_set_up") {
    description =
      "Encrypt your direct messages so only you and the person you're talking to can read them — the server only ever stores ciphertext. Protected by a passphrase you choose; you'll need it on other devices.";
    badge = { text: "Off", on: false };
    actions = <PrimaryButton onClick={() => openModal("setup")}>Set up encryption</PrimaryButton>;
  } else if (status === "locked") {
    description =
      "Your encryption keys are backed up, but this device hasn't unlocked them. Enter your passphrase to read and send encrypted messages here.";
    badge = { text: "Locked", on: false };
    actions = (
      <>
        <SecondaryButton onClick={() => openModal("reset")}>Reset keys</SecondaryButton>
        <PrimaryButton onClick={() => openModal("unlock")}>Unlock</PrimaryButton>
      </>
    );
  } else {
    description =
      "Messages with anyone who has also set up encryption are sealed on your device; the server only sees ciphertext. Your fingerprint below is what others compare against your profile.";
    badge = { text: "On", on: true };
    actions = (
      <>
        <SecondaryButton onClick={() => openModal("reset")}>Reset keys</SecondaryButton>
        <SecondaryButton onClick={() => openModal("change")}>Change passphrase</SecondaryButton>
      </>
    );
  }

  return (
    <div className="rounded-md border border-border-divider bg-bg-light px-4 py-3.5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-[14px] font-medium text-text-primary">
            <span className="text-text-secondary">
              <LockGlyph size={14} />
            </span>
            End-to-end encrypted DMs
            {badge && (
              <span
                className={`rounded-sm px-1.5 py-px font-channel text-[10px] font-semibold uppercase tracking-[0.06em] ${
                  badge.on ? "bg-success/15 text-success" : "bg-text-muted/15 text-text-muted"
                }`}
              >
                {badge.text}
              </span>
            )}
          </div>
          <div className="mt-1 text-[12px] leading-[1.55] text-text-muted">{description}</div>
          {status === "ready" && fingerprint && (
            <div className="mt-2.5">
              <div className="text-[10px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                Your fingerprint
              </div>
              <div className="mt-1 select-text font-mono text-[11px] tracking-[0.06em] text-text-primary">
                {fingerprint}
              </div>
            </div>
          )}
        </div>
      </div>
      {actions && <div className="mt-3 flex justify-end gap-2">{actions}</div>}
    </div>
  );
}

function PrimaryButton({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
    >
      {children}
    </button>
  );
}

function SecondaryButton({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-sm border border-border px-4 py-2 text-[13px] font-medium text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
    >
      {children}
    </button>
  );
}
