import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function DeepLinkJoinModal() {
  const pendingInvite = useChatStore((s) => s.pendingInvite);
  const setPendingInvite = useChatStore((s) => s.setPendingInvite);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const authError = useUiStore((s) => s.authError);
  const setAuthError = useUiStore((s) => s.setAuthError);

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
    if (!pendingInvite || !authError) return;
    const targetId = `${pendingInvite.host}:${pendingInvite.port}`;
    if (authError.serverId === targetId) {
      setJoining(false);
    }
  }, [pendingInvite, authError]);

  if (!pendingInvite) return null;

  const { host, port, code } = pendingInvite;
  const serverId = `${host}:${port}`;

  const close = () => {
    setPendingInvite(null);
    setAuthError(null);
    setLocalError(null);
  };

  const handleJoin = async () => {
    setJoining(true);
    setLocalError(null);
    setAuthError(null);
    try {
      await invoke("redeem_invite", {
        serverId,
        host,
        port,
        inviteCode: code,
      });
      setActiveServer(serverId);
      setActiveView("server");
      // Leave the modal open briefly so the user sees success; the
      // community_auth_responded event will either clear it (success path
      // triggered via close below) or surface an authError (handled above).
      setPendingInvite(null);
    } catch (err) {
      setLocalError(String(err));
      setJoining(false);
    }
  };

  const inlineError =
    authError && authError.serverId === serverId ? authError : null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={close}
    >
      <div
        className="w-full max-w-sm rounded-2xl border border-border bg-bg-secondary p-6 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-text-bright">
            Join server
          </h2>
          <button
            onClick={close}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            ×
          </button>
        </div>
        <p className="mb-2 text-sm text-text-secondary">
          You have been invited to:
        </p>
        <div className="mb-1 rounded-xl border border-border bg-bg-dark px-3 py-2.5">
          <div className="font-mono text-sm text-text-bright">
            {host}:{port}
          </div>
          <div className="mt-1 font-mono text-[11px] tracking-wider text-text-muted">
            Code: {code}
          </div>
        </div>

        {(inlineError || localError) && (
          <p className="mt-3 text-xs text-error">
            {inlineError?.message ?? localError}
          </p>
        )}

        <div className="mt-4 flex gap-2">
          <button
            onClick={close}
            className="flex-1 rounded-lg border border-border bg-bg-primary px-4 py-2 text-sm font-semibold text-text-primary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          <button
            onClick={handleJoin}
            disabled={joining}
            className="flex-1 rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {joining ? "Joining..." : "Accept"}
          </button>
        </div>
      </div>
    </div>
  );
}
