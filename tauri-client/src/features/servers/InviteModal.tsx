import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import type { ServerInvite } from "../../types";

const EXPIRY_OPTIONS: { label: string; seconds: number }[] = [
  { label: "1 hour", seconds: 3600 },
  { label: "1 day", seconds: 86400 },
  { label: "7 days", seconds: 604800 },
  { label: "30 days", seconds: 2592000 },
  { label: "Never", seconds: 0 },
];

const MAX_USES_OPTIONS: { label: string; value: number }[] = [
  { label: "Unlimited", value: 0 },
  { label: "1 use", value: 1 },
  { label: "5 uses", value: 5 },
  { label: "25 uses", value: 25 },
  { label: "100 uses", value: 100 },
];

function formatAbsolute(epoch: number): string {
  if (epoch === 0) return "Never";
  const d = new Date(epoch * 1000);
  return d.toLocaleString();
}

function formatUses(invite: ServerInvite): string {
  if (invite.maxUses === 0) return `${invite.uses} / ∞`;
  return `${invite.uses} / ${invite.maxUses}`;
}

export default function InviteModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const serverOwner = useChatStore((s) => s.serverOwner);
  const invitesByServer = useChatStore((s) => s.invitesByServer);
  const servers = useChatStore((s) => s.servers);
  const currentUser = useAuthStore((s) => s.username);

  const [expirySec, setExpirySec] = useState(604800);
  const [maxUses, setMaxUses] = useState(0);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copiedCode, setCopiedCode] = useState<string | null>(null);

  const invites = activeServerId ? invitesByServer[activeServerId] ?? [] : [];
  const server = useMemo(
    () => servers.find((s) => s.id === activeServerId) ?? null,
    [servers, activeServerId]
  );
  const isOwner =
    !!activeServerId && !!currentUser && serverOwner[activeServerId] === currentUser;

  useEffect(() => {
    if (activeModal === "invite-manage" && activeServerId && isOwner) {
      invoke("list_invites", { serverId: activeServerId }).catch((err) =>
        setError(String(err))
      );
    }
  }, [activeModal, activeServerId, isOwner]);

  if (activeModal !== "invite-manage" || !activeServerId) return null;

  if (!isOwner) {
    return (
      <div
        className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
        onClick={closeModal}
      >
        <div
          className="w-full max-w-sm rounded-2xl border border-border bg-bg-secondary p-6 shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          <h2 className="mb-2 text-lg font-semibold text-text-bright">
            Invites
          </h2>
          <p className="text-sm text-text-secondary">
            Only the server owner can manage invites.
          </p>
          <button
            onClick={closeModal}
            className="mt-4 w-full rounded-lg bg-surface-hover px-4 py-2 text-sm font-semibold text-text-primary"
          >
            Close
          </button>
        </div>
      </div>
    );
  }

  const buildInviteLink = (code: string): string => {
    const host = server?.hostIp ?? "";
    const port = server?.port ?? 0;
    return `decibell://invite/${host}:${port}/${code}`;
  };

  const handleCopy = async (code: string) => {
    const link = buildInviteLink(code);
    try {
      await navigator.clipboard.writeText(link);
      setCopiedCode(code);
      setTimeout(() => setCopiedCode((c) => (c === code ? null : c)), 1500);
    } catch (err) {
      setError(String(err));
    }
  };

  const handleCopyCode = async (code: string) => {
    try {
      await navigator.clipboard.writeText(code);
      setCopiedCode(`raw:${code}`);
      setTimeout(
        () => setCopiedCode((c) => (c === `raw:${code}` ? null : c)),
        1500
      );
    } catch (err) {
      setError(String(err));
    }
  };

  const handleCreate = async () => {
    setCreating(true);
    setError(null);
    try {
      const expiresAt =
        expirySec === 0 ? 0 : Math.floor(Date.now() / 1000) + expirySec;
      await invoke("create_invite", {
        serverId: activeServerId,
        expiresAt,
        maxUses,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setCreating(false);
    }
  };

  const handleRevoke = async (code: string) => {
    try {
      await invoke("revoke_invite", { serverId: activeServerId, code });
    } catch (err) {
      setError(String(err));
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-xl flex-col rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center justify-between border-b border-border px-6 py-4">
          <h2 className="text-lg font-semibold text-text-bright">
            Invites
          </h2>
          <button
            onClick={closeModal}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            ×
          </button>
        </div>

        {/* Create form */}
        <div className="shrink-0 border-b border-border px-6 py-4">
          <h3 className="mb-3 font-channel text-[11px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Create Invite
          </h3>
          <div className="mb-3 grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-xs text-text-secondary">
                Expires
              </label>
              <select
                value={expirySec}
                onChange={(e) => setExpirySec(parseInt(e.target.value, 10))}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
              >
                {EXPIRY_OPTIONS.map((opt) => (
                  <option key={opt.seconds} value={opt.seconds}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="mb-1 block text-xs text-text-secondary">
                Max uses
              </label>
              <select
                value={maxUses}
                onChange={(e) => setMaxUses(parseInt(e.target.value, 10))}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
              >
                {MAX_USES_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <button
            onClick={handleCreate}
            disabled={creating}
            className="w-full rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {creating ? "Creating..." : "Create Invite"}
          </button>
          {error && <p className="mt-2 text-xs text-error">{error}</p>}
        </div>

        {/* Invite list */}
        <div className="flex-1 overflow-y-auto px-6 py-4">
          <h3 className="mb-3 font-channel text-[11px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Active Invites ({invites.length})
          </h3>
          {invites.length === 0 ? (
            <p className="py-6 text-center text-sm text-text-muted">
              No invites yet. Create one above.
            </p>
          ) : (
            <div className="space-y-2">
              {invites.map((invite) => (
                <div
                  key={invite.code}
                  className="rounded-xl border border-border bg-bg-dark p-3"
                >
                  <div className="flex items-center justify-between gap-3">
                    <button
                      onClick={() => handleCopyCode(invite.code)}
                      title="Copy code"
                      className="group flex items-center gap-2 rounded-md bg-bg-primary px-2.5 py-1 font-mono text-[13px] font-semibold tracking-wider text-text-bright transition-colors hover:bg-surface-hover"
                    >
                      {invite.code}
                      <span className="text-[10px] text-text-muted group-hover:text-text-secondary">
                        {copiedCode === `raw:${invite.code}`
                          ? "Copied!"
                          : "Copy"}
                      </span>
                    </button>
                    <div className="flex gap-2">
                      <button
                        onClick={() => handleCopy(invite.code)}
                        className="rounded-md border border-border bg-bg-primary px-3 py-1 text-xs font-medium text-text-primary transition-colors hover:border-accent hover:text-accent-bright"
                      >
                        {copiedCode === invite.code ? "Copied link!" : "Copy link"}
                      </button>
                      <button
                        onClick={() => handleRevoke(invite.code)}
                        className="rounded-md border border-error/40 bg-error/10 px-3 py-1 text-xs font-medium text-error transition-colors hover:bg-error/20"
                      >
                        Revoke
                      </button>
                    </div>
                  </div>
                  <div className="mt-2 flex flex-wrap gap-4 text-[11px] text-text-muted">
                    <span>By {invite.createdBy}</span>
                    <span>Uses: {formatUses(invite)}</span>
                    <span>Expires: {formatAbsolute(invite.expiresAt)}</span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
