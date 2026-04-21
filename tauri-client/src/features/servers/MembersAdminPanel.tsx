import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToGradient } from "../../utils/colors";

type Tab = "members" | "bans";

function formatJoined(epoch: number): string {
  if (!epoch) return "—";
  const d = new Date(epoch * 1000);
  return d.toLocaleDateString();
}

export default function MembersAdminPanel() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const membersByServer = useChatStore((s) => s.membersByServer);
  const bansByServer = useChatStore((s) => s.bansByServer);
  const serverOwner = useChatStore((s) => s.serverOwner);
  const currentUser = useAuthStore((s) => s.username);

  const [tab, setTab] = useState<Tab>("members");
  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<
    | { kind: "kick" | "ban" | "leave"; username?: string }
    | null
  >(null);

  const members = activeServerId ? membersByServer[activeServerId] ?? [] : [];
  const bans = activeServerId ? bansByServer[activeServerId] ?? [] : [];
  const owner = activeServerId ? serverOwner[activeServerId] : undefined;
  const isOwner = !!currentUser && !!owner && currentUser === owner;

  useEffect(() => {
    if (activeModal === "members-manage" && activeServerId) {
      invoke("list_members", { serverId: activeServerId }).catch((err) =>
        setError(String(err))
      );
    }
  }, [activeModal, activeServerId]);

  if (activeModal !== "members-manage" || !activeServerId) return null;

  const runKick = async (username: string) => {
    setPendingAction(`kick:${username}`);
    setError(null);
    try {
      await invoke("kick_member", { serverId: activeServerId, username });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirm(null);
    }
  };

  const runBan = async (username: string) => {
    setPendingAction(`ban:${username}`);
    setError(null);
    try {
      await invoke("ban_member", { serverId: activeServerId, username });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirm(null);
    }
  };

  const runLeave = async () => {
    setPendingAction("leave");
    setError(null);
    try {
      await invoke("leave_server", { serverId: activeServerId });
      closeModal();
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirm(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 transition-colors duration-300"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-[480px] animate-[cardIn_0.25s_ease] flex-col overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)]"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex shrink-0 items-center justify-between px-6 pb-0 pt-5">
          <h2 className="font-display text-[18px] font-semibold text-text-primary">
            Server Members
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

        {/* Tabs */}
        <div className="flex shrink-0 gap-1 border-b border-border-divider px-6 pt-4">
          <button
            onClick={() => setTab("members")}
            className={`-mb-px border-b-2 px-4 py-2.5 text-[13px] font-medium transition-colors ${
              tab === "members"
                ? "border-accent text-text-primary"
                : "border-transparent text-text-muted hover:text-text-secondary"
            }`}
          >
            Members ({members.length})
          </button>
          {isOwner && (
            <button
              onClick={() => setTab("bans")}
              className={`-mb-px border-b-2 px-4 py-2.5 text-[13px] font-medium transition-colors ${
                tab === "bans"
                  ? "border-accent text-text-primary"
                  : "border-transparent text-text-muted hover:text-text-secondary"
              }`}
            >
              Banned ({bans.length})
            </button>
          )}
        </div>

        {error && (
          <p className="shrink-0 px-6 pt-3 text-[12px] text-error">{error}</p>
        )}

        {/* List */}
        <div className="flex-1 overflow-y-auto px-5 py-4 scrollbar-thin">
          {tab === "members" ? (
            members.length === 0 ? (
              <div className="flex flex-col items-center gap-2.5 py-8 text-text-muted">
                <div className="flex h-10 w-10 items-center justify-center rounded-full bg-text-muted/10">
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
                    <circle cx="9" cy="7" r="4" />
                  </svg>
                </div>
                <span className="text-[13px]">No members yet.</span>
              </div>
            ) : (
              <div className="flex flex-col gap-1">
                {members.map((m) => {
                  const isSelf = m.username === currentUser;
                  const canModerate = isOwner && !m.isOwner && !isSelf;
                  const displayName = m.nickname || m.username;
                  return (
                    <div
                      key={m.username}
                      className="flex items-center gap-3 rounded-[10px] px-3 py-2.5 transition-colors hover:bg-surface-hover"
                    >
                      {/* Avatar */}
                      <div className="relative shrink-0">
                        <div
                          className="flex h-9 w-9 items-center justify-center rounded-lg text-[14px] font-semibold text-white"
                          style={{ background: stringToGradient(m.username) }}
                        >
                          {m.username.charAt(0).toUpperCase()}
                        </div>
                        <div
                          className={`absolute -bottom-px -right-px h-[11px] w-[11px] rounded-full border-[2.5px] border-bg-dark ${
                            m.isOnline ? "bg-success" : "bg-text-muted"
                          }`}
                        />
                      </div>

                      {/* Info */}
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-1.5">
                          <span className="truncate text-[13px] font-medium text-text-primary">
                            {displayName}
                          </span>
                          {m.isOwner && (
                            <span className="rounded bg-warning/15 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.04em] text-warning">
                              Owner
                            </span>
                          )}
                          {isSelf && (
                            <span className="rounded bg-accent-soft px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.04em] text-accent-bright">
                              You
                            </span>
                          )}
                        </div>
                        <div className="mt-0.5 text-[11px] text-text-muted">
                          Joined {formatJoined(m.joinedAt)}
                        </div>
                      </div>

                      {/* Moderation actions */}
                      {canModerate && (
                        <div className="flex shrink-0 gap-1.5">
                          <button
                            onClick={() =>
                              setConfirm({ kind: "kick", username: m.username })
                            }
                            disabled={pendingAction === `kick:${m.username}`}
                            className="rounded-md bg-warning/10 px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-warning/20 hover:text-warning disabled:opacity-50"
                          >
                            Kick
                          </button>
                          <button
                            onClick={() =>
                              setConfirm({ kind: "ban", username: m.username })
                            }
                            disabled={pendingAction === `ban:${m.username}`}
                            className="rounded-md bg-error/10 px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-error/20 hover:text-error disabled:opacity-50"
                          >
                            Ban
                          </button>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )
          ) : bans.length === 0 ? (
            <div className="flex flex-col items-center gap-2.5 py-8 text-text-muted">
              <div className="flex h-10 w-10 items-center justify-center rounded-full bg-text-muted/10">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <circle cx="12" cy="12" r="10" />
                  <line x1="4.93" y1="4.93" x2="19.07" y2="19.07" />
                </svg>
              </div>
              <span className="text-[13px]">No bans.</span>
            </div>
          ) : (
            <div className="flex flex-col gap-1">
              {bans.map((username) => (
                <div
                  key={username}
                  className="flex items-center gap-3 rounded-[10px] px-3 py-2.5 transition-colors hover:bg-surface-hover"
                >
                  <div
                    className="flex h-9 w-9 items-center justify-center rounded-lg text-[14px] font-semibold text-white opacity-50"
                    style={{ background: stringToGradient(username) }}
                  >
                    {username.charAt(0).toUpperCase()}
                  </div>
                  <span className="flex-1 truncate text-[13px] font-medium text-text-secondary">
                    {username}
                  </span>
                  <span className="rounded bg-error/15 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.04em] text-error">
                    Banned
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Footer: leave button for non-owners */}
        {!isOwner && (
          <div className="shrink-0 border-t border-border-divider px-6 py-4">
            <button
              onClick={() => setConfirm({ kind: "leave" })}
              disabled={pendingAction === "leave"}
              className="w-full rounded-[10px] border border-error/20 bg-error/10 py-2.5 text-[13px] font-semibold text-error transition-colors hover:bg-error/20 disabled:opacity-50"
            >
              Leave Server
            </button>
          </div>
        )}
      </div>

      {/* Confirmation dialog */}
      {confirm && (
        <div
          className="fixed inset-0 z-10 flex items-center justify-center bg-black/50"
          onClick={(e) => {
            e.stopPropagation();
            setConfirm(null);
          }}
        >
          <div
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-2xl border border-border bg-bg-dark p-6 shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)]"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              {confirm.kind === "kick" && `Kick ${confirm.username}?`}
              {confirm.kind === "ban" && `Ban ${confirm.username}?`}
              {confirm.kind === "leave" && "Leave this server?"}
            </h3>
            <p className="mb-5 text-[13px] leading-relaxed text-text-secondary">
              {confirm.kind === "kick" &&
                "They will be disconnected but can rejoin with a valid invite."}
              {confirm.kind === "ban" &&
                "They will be disconnected and prevented from rejoining."}
              {confirm.kind === "leave" &&
                "You will need a new invite to rejoin."}
            </p>
            <div className="flex gap-2.5">
              <button
                onClick={() => setConfirm(null)}
                className="flex-1 rounded-[10px] bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
              >
                Cancel
              </button>
              <button
                onClick={() => {
                  if (confirm.kind === "kick" && confirm.username)
                    runKick(confirm.username);
                  else if (confirm.kind === "ban" && confirm.username)
                    runBan(confirm.username);
                  else if (confirm.kind === "leave") runLeave();
                }}
                disabled={!!pendingAction}
                className="flex-1 rounded-[10px] bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85 disabled:opacity-50"
              >
                {pendingAction ? "Working..." : "Confirm"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
