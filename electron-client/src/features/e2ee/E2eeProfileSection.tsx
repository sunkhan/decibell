import { useE2eeStore } from "../../stores/e2eeStore";
import { usePeerE2ee } from "./usePeerE2ee";

/// Profile-popup block: the conversation's safety number (identical on
/// both users' screens) for out-of-band verification, or a note that the
/// peer hasn't set up encryption. Only when our own keys are ready — the
/// number needs both identities.
export default function E2eeProfileSection({ peer }: { peer: string }) {
  const status = useE2eeStore((s) => s.status);
  const info = usePeerE2ee(status === "ready" ? peer : null);
  if (status !== "ready" || !info) return null;

  return (
    <>
      <div className="mx-4 my-3 h-px bg-border-divider" />
      <div className="mx-4 mb-3">
        <div className="mb-1.5 text-[11px] font-medium text-text-secondary">Encryption</div>
        {info.hasKeys ? (
          <>
            <div className="text-[11px] text-text-muted">
              Safety number. Compare it with {peer} in person or on a call. A match means nobody
              is sitting between you.
            </div>
            <div className="mt-2 grid grid-cols-4 gap-x-2 gap-y-0.5 rounded-sm bg-bg-dark px-2.5 py-2 font-mono text-[11px] tracking-[0.06em] text-text-primary">
              {info.safetyNumber.split(" ").map((group, i) => (
                <span key={i}>{group}</span>
              ))}
            </div>
            {info.changedAt > 0 && (
              <div className="mt-1.5 text-[11px] text-warning">
                Keys changed {new Date(info.changedAt * 1000).toLocaleString()}
              </div>
            )}
          </>
        ) : (
          <div className="text-[11px] text-text-muted">
            {peer} hasn't set up encryption, your messages to them aren't encrypted yet.
          </div>
        )}
      </div>
    </>
  );
}
