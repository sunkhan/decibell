import { useEffect, useState } from "react";
import { handleCertMismatch } from "../../lib/certMismatch";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useInviteResolveStore } from "../../stores/inviteResolveStore";

// Confirm-then-join for an invite that arrived as a deep link or a
// clicked chat link. Links carry only the code, so the endpoint comes
// from central (inviteResolveStore → resolve_invite_code) along with
// the server's name and description for the preview; an older
// host:port link can still join even when central is unreachable.
export default function DeepLinkJoinModal() {
  const pendingInvite = useChatStore((s) => s.pendingInvite);
  const setPendingInvite = useChatStore((s) => s.setPendingInvite);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const authError = useUiStore((s) => s.authError);
  const setAuthError = useUiStore((s) => s.setAuthError);

  const code = pendingInvite ? pendingInvite.code.toUpperCase() : "";
  const entry = useInviteResolveStore((s) => (code ? s.entries[code] : undefined));
  useEffect(() => {
    if (code) useInviteResolveStore.getState().request(code);
  }, [code]);
  const resolved = entry?.status === "done" ? entry.resolved : null;
  const host = pendingInvite?.host ?? resolved?.host ?? "";
  const port = pendingInvite?.port ?? resolved?.port ?? 0;
  const serverId = host && port ? `${host}:${port}` : "";

  const [joining, setJoining] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  // Clear auth errors on open so stale errors from a previous attempt don't linger.
  useEffect(() => {
    if (pendingInvite) {
      setAuthError(null);
      setLocalError(null);
    }
  }, [pendingInvite, setAuthError]);

  // If the server reports an auth failure for this invite's server, surface
  // it inline and stop the spinner.
  useEffect(() => {
    if (!pendingInvite || !authError || !serverId) return;
    if (authError.serverId === serverId) setJoining(false);
  }, [pendingInvite, authError, serverId]);

  if (!pendingInvite) return null;

  const invalid = entry?.status === "done" && entry.invalid;
  const resolving = !entry || entry.status === "loading";
  const resolveError =
    entry?.status === "done" && entry.resolved === null && !entry.invalid ? entry.error : null;
  const canJoin = serverId !== "" && !joining && !invalid;
  const title =
    resolved?.serverName ||
    (serverId ? serverId : resolving ? "Resolving invite…" : "Couldn't resolve invite");

  const close = () => {
    setPendingInvite(null);
    setAuthError(null);
    setLocalError(null);
  };

  const handleJoin = async () => {
    if (!serverId) return;
    setJoining(true);
    setLocalError(null);
    setAuthError(null);
    try {
      await invoke("redeem_invite", {
        serverId,
        host,
        port,
        inviteCode: pendingInvite.code,
      });
      setActiveServer(serverId);
      setActiveView("server");
      setPendingInvite(null);
    } catch (err) {
      if (!handleCertMismatch(err, handleJoin)) setLocalError(String(err));
      setJoining(false);
    }
  };

  const inlineError =
    authError && serverId && authError.serverId === serverId ? authError : null;
  const errorText =
    inlineError?.message ??
    localError ??
    (invalid ? "This invite is unknown, expired, or used up." : null) ??
    (resolveError && !serverId ? resolveError : null);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={close}
    >
      <div
        className="w-full max-w-sm rounded-xl border border-border bg-bg-secondary p-6 shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-text-bright">
            Join server
          </h2>
          <button
            onClick={close}
            className="flex h-8 w-8 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            ×
          </button>
        </div>
        <p className="mb-2 text-sm text-text-secondary">
          You have been invited to:
        </p>
        <div className="mb-1 rounded-lg border border-border bg-bg-dark px-3 py-2.5">
          <div className={`truncate text-sm font-semibold ${invalid ? "text-text-muted" : "text-text-bright"}`}>
            {invalid ? "Invalid invite" : title}
          </div>
          {resolved?.serverDescription && !invalid && (
            <div className="mt-0.5 line-clamp-2 text-xs text-text-secondary">
              {resolved.serverDescription}
            </div>
          )}
          <div className="mt-1 font-mono text-[11px] tracking-[0.07em] text-text-muted">
            {resolved && resolved.memberCount > 0 ? `${resolved.memberCount} members · ` : ""}
            Code: {pendingInvite.code}
          </div>
        </div>

        {errorText && <p className="mt-3 text-xs text-error">{errorText}</p>}

        <div className="mt-4 flex gap-2">
          <button
            onClick={close}
            className="flex-1 rounded-md border border-border bg-bg-primary px-4 py-2 text-sm font-semibold text-text-primary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          <button
            onClick={handleJoin}
            disabled={!canJoin}
            className="flex-1 rounded-md bg-accent px-4 py-2 text-sm font-semibold text-on-accent transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {joining ? "Joining..." : resolving && !serverId ? "Resolving…" : "Accept"}
          </button>
        </div>
      </div>
    </div>
  );
}
