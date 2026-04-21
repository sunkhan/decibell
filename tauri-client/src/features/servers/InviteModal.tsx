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
  const [maxUsesUnlimited, setMaxUsesUnlimited] = useState(true);
  const [maxUsesInput, setMaxUsesInput] = useState("10");
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
        className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
        style={{ backgroundColor: "rgba(0,0,0,0.65)" }}
        onClick={closeModal}
      >
        <div
          className="w-full max-w-sm rounded-2xl border border-border bg-bg-dark p-6 shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)]"
          onClick={(e) => e.stopPropagation()}
        >
          <h2 className="mb-2 font-display text-lg font-semibold text-text-primary">
            Invites
          </h2>
          <p className="text-[13px] text-text-secondary">
            Only the server owner can manage invites.
          </p>
          <button
            onClick={closeModal}
            className="mt-4 w-full rounded-[10px] bg-bg-light px-4 py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
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

  const parsedMaxUses = parseInt(maxUsesInput, 10);
  const maxUsesValid =
    maxUsesUnlimited ||
    (!isNaN(parsedMaxUses) && parsedMaxUses >= 1 && parsedMaxUses <= 1000);

  const handleCreate = async () => {
    setCreating(true);
    setError(null);
    try {
      const expiresAt =
        expirySec === 0 ? 0 : Math.floor(Date.now() / 1000) + expirySec;
      const maxUses = maxUsesUnlimited ? 0 : parsedMaxUses;
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

  const stepMaxUses = (delta: number) => {
    const current = parseInt(maxUsesInput, 10) || 0;
    const next = Math.max(1, Math.min(1000, current + delta));
    setMaxUsesInput(String(next));
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: "rgba(0,0,0,0.65)" }}
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-[440px] animate-[cardIn_0.25s_ease] flex-col overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)]"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex shrink-0 items-center justify-between border-b border-border-divider px-6 py-5">
          <h2 className="font-display text-[18px] font-semibold text-text-primary">
            Invites
          </h2>
          <button
            onClick={closeModal}
            className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        {/* Create form */}
        <div className="shrink-0 px-6 py-5">
          <div className="mb-3.5 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Create invite
          </div>

          <div className="mb-3.5 grid grid-cols-2 gap-3">
            {/* Expires */}
            <div>
              <label className="mb-1.5 block text-[12px] font-medium text-text-secondary">
                Expires
              </label>
              <div className="relative">
                <select
                  value={expirySec}
                  onChange={(e) => setExpirySec(parseInt(e.target.value, 10))}
                  className="w-full appearance-none rounded-lg border border-border bg-bg-lighter px-3 py-2.5 pr-9 text-[13px] text-text-primary outline-none transition-all hover:border-white/[0.1] focus:border-accent focus:shadow-[0_0_0_2px_var(--color-accent-soft)]"
                >
                  {EXPIRY_OPTIONS.map((opt) => (
                    <option key={opt.seconds} value={opt.seconds} className="bg-bg-lighter">
                      {opt.label}
                    </option>
                  ))}
                </select>
                <div className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-text-muted">
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><path d="M6 9l6 6 6-6" /></svg>
                </div>
              </div>
            </div>

            {/* Max uses */}
            <div>
              <label className="mb-1.5 block text-[12px] font-medium text-text-secondary">
                Max uses
              </label>
              <div className="flex h-[40px] items-center overflow-hidden rounded-lg border border-border bg-bg-lighter transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
                {/* Unlimited toggle */}
                <label className="flex h-full shrink-0 cursor-pointer items-center gap-[7px] border-r border-border-divider px-3 transition-colors hover:bg-surface-hover">
                  <input
                    type="checkbox"
                    checked={maxUsesUnlimited}
                    onChange={(e) => setMaxUsesUnlimited(e.target.checked)}
                    className="hidden"
                  />
                  <div
                    className={`flex h-4 w-4 shrink-0 items-center justify-center rounded transition-all ${
                      maxUsesUnlimited
                        ? "border-accent bg-accent"
                        : "border-[1.5px] border-white/[0.12] bg-transparent"
                    }`}
                  >
                    <svg
                      width="10"
                      height="10"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="white"
                      strokeWidth="3"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      className={`transition-opacity ${maxUsesUnlimited ? "opacity-100" : "opacity-0"}`}
                    >
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                  </div>
                  <span className="whitespace-nowrap text-[12px] font-medium text-text-secondary">
                    Unlimited
                  </span>
                </label>

                {/* Infinity or number input */}
                {maxUsesUnlimited ? (
                  <div className="flex flex-1 items-center justify-center text-[18px] text-text-muted">
                    ∞
                  </div>
                ) : (
                  <>
                    <input
                      type="text"
                      inputMode="numeric"
                      value={maxUsesInput}
                      onChange={(e) => {
                        const digits = e.target.value.replace(/\D/g, "");
                        if (digits === "") {
                          setMaxUsesInput("");
                          return;
                        }
                        const clamped = Math.min(1000, parseInt(digits, 10));
                        setMaxUsesInput(String(clamped));
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "ArrowUp") {
                          e.preventDefault();
                          stepMaxUses(1);
                        } else if (e.key === "ArrowDown") {
                          e.preventDefault();
                          stepMaxUses(-1);
                        }
                      }}
                      onWheel={(e) => {
                        if (e.deltaY < 0) stepMaxUses(1);
                        else if (e.deltaY > 0) stepMaxUses(-1);
                      }}
                      placeholder="10"
                      autoFocus
                      className="h-full min-w-0 flex-1 bg-transparent px-3 text-[13px] text-text-primary outline-none placeholder:text-text-faint"
                    />
                    <div className="flex h-full shrink-0 flex-col border-l border-border-divider">
                      <button
                        onClick={() => stepMaxUses(1)}
                        className="flex w-7 flex-1 items-center justify-center border-b border-border-divider text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
                      >
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><path d="M18 15l-6-6-6 6" /></svg>
                      </button>
                      <button
                        onClick={() => stepMaxUses(-1)}
                        className="flex w-7 flex-1 items-center justify-center text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
                      >
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><path d="M6 9l6 6 6-6" /></svg>
                      </button>
                    </div>
                  </>
                )}
              </div>
            </div>
          </div>

          <button
            onClick={handleCreate}
            disabled={creating || !maxUsesValid}
            className="w-full rounded-[10px] bg-accent py-[11px] text-[13px] font-semibold text-white shadow-[0_2px_12px_rgba(56,143,255,0.22)] transition-all hover:bg-accent-hover hover:shadow-[0_4px_20px_rgba(56,143,255,0.3)] active:scale-[0.98] disabled:opacity-50 disabled:shadow-none"
          >
            {creating ? "Creating..." : "Create Invite"}
          </button>
          {error && (
            <p className="mt-2.5 text-[12px] text-error">{error}</p>
          )}
        </div>

        {/* Invite list */}
        <div className="flex-1 overflow-y-auto border-t border-border-divider px-6 py-5">
          <div className="mb-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Active invites ({invites.length})
          </div>

          {invites.length === 0 ? (
            <div className="flex flex-col items-center gap-2.5 py-5">
              <div className="flex h-10 w-10 items-center justify-center rounded-full bg-text-muted/15 text-text-muted">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
                  <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
                </svg>
              </div>
              <span className="text-[13px] text-text-muted">
                No invites yet. Create one above.
              </span>
            </div>
          ) : (
            <div className="flex flex-col gap-2">
              {invites.map((invite) => (
                <div
                  key={invite.code}
                  className="rounded-[10px] border border-border-divider bg-bg-light p-3.5"
                >
                  {/* Top row: code + actions */}
                  <div className="flex items-center gap-2.5">
                    <button
                      onClick={() => handleCopyCode(invite.code)}
                      title="Copy code"
                      className="group flex items-center gap-2 rounded-md bg-bg-mid px-2.5 py-1.5 font-mono text-[13px] font-semibold tracking-wider text-accent-bright transition-colors hover:bg-bg-lighter"
                    >
                      {invite.code}
                      <span className="text-[10px] font-normal tracking-normal text-text-muted transition-colors group-hover:text-text-secondary">
                        {copiedCode === `raw:${invite.code}` ? "Copied!" : "Copy"}
                      </span>
                    </button>

                    <div className="ml-auto flex gap-1.5">
                      <button
                        onClick={() => handleCopy(invite.code)}
                        className="rounded-md bg-accent-soft px-3 py-1.5 text-[11px] font-medium text-accent-bright transition-colors hover:bg-accent-mid"
                      >
                        {copiedCode === invite.code ? "Copied!" : "Copy link"}
                      </button>
                      <button
                        onClick={() => handleRevoke(invite.code)}
                        className="rounded-md bg-error/10 px-3 py-1.5 text-[11px] font-medium text-error transition-colors hover:bg-error/20"
                      >
                        Revoke
                      </button>
                    </div>
                  </div>

                  {/* Meta row */}
                  <div className="mt-2.5 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-text-muted">
                    <span className="flex items-center gap-1.5">
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                        <circle cx="12" cy="8" r="4" />
                        <path d="M20 21a8 8 0 1 0-16 0" />
                      </svg>
                      {invite.createdBy}
                    </span>
                    <span className="flex items-center gap-1.5">
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                        <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
                        <circle cx="9" cy="7" r="4" />
                        <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
                        <path d="M16 3.13a4 4 0 0 1 0 7.75" />
                      </svg>
                      {formatUses(invite)}
                    </span>
                    <span className="flex items-center gap-1.5">
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <circle cx="12" cy="12" r="10" />
                        <polyline points="12 6 12 12 16 14" />
                      </svg>
                      {formatAbsolute(invite.expiresAt)}
                    </span>
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
