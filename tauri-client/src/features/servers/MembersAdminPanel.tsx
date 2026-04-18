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
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-xl flex-col rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center justify-between border-b border-border px-6 py-4">
          <h2 className="text-lg font-semibold text-text-bright">
            Server Members
          </h2>
          <button
            onClick={closeModal}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            ×
          </button>
        </div>

        {/* Tabs */}
        <div className="flex shrink-0 gap-1 border-b border-border px-6 pt-3">
          <button
            onClick={() => setTab("members")}
            className={`rounded-t-md px-3 py-1.5 text-sm font-medium transition-colors ${
              tab === "members"
                ? "bg-bg-primary text-text-bright"
                : "text-text-muted hover:text-text-primary"
            }`}
          >
            Members ({members.length})
          </button>
          {isOwner && (
            <button
              onClick={() => setTab("bans")}
              className={`rounded-t-md px-3 py-1.5 text-sm font-medium transition-colors ${
                tab === "bans"
                  ? "bg-bg-primary text-text-bright"
                  : "text-text-muted hover:text-text-primary"
              }`}
            >
              Banned ({bans.length})
            </button>
          )}
        </div>

        {error && (
          <p className="shrink-0 px-6 pt-3 text-xs text-error">{error}</p>
        )}

        <div className="flex-1 overflow-y-auto px-6 py-4">
          {tab === "members" ? (
            members.length === 0 ? (
              <p className="py-6 text-center text-sm text-text-muted">
                No members yet.
              </p>
            ) : (
              <div className="space-y-1">
                {members.map((m) => {
                  const isSelf = m.username === currentUser;
                  const canModerate = isOwner && !m.isOwner && !isSelf;
                  const displayName = m.nickname || m.username;
                  return (
                    <div
                      key={m.username}
                      className="flex items-center gap-3 rounded-lg px-2 py-2 hover:bg-surface-hover"
                    >
                      <div className="relative shrink-0">
                        <div
                          className="flex h-9 w-9 items-center justify-center rounded-lg text-[13px] font-bold text-white"
                          style={{ background: stringToGradient(m.username) }}
                        >
                          {m.username.charAt(0).toUpperCase()}
                        </div>
                        <div
                          className={`absolute -bottom-px -right-px h-[10px] w-[10px] rounded-full border-[2px] border-bg-secondary ${
                            m.isOnline ? "bg-success" : "bg-text-muted"
                          }`}
                        />
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span className="truncate text-sm font-semibold text-text-primary">
                            {displayName}
                          </span>
                          {m.isOwner && (
                            <span className="rounded bg-warning/15 px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider text-warning">
                              Owner
                            </span>
                          )}
                          {isSelf && (
                            <span className="rounded bg-accent-soft px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider text-accent-bright">
                              You
                            </span>
                          )}
                        </div>
                        <div className="text-[11px] text-text-muted">
                          Joined {formatJoined(m.joinedAt)}
                        </div>
                      </div>
                      {canModerate && (
                        <div className="flex shrink-0 gap-1.5">
                          <button
                            onClick={() =>
                              setConfirm({ kind: "kick", username: m.username })
                            }
                            disabled={pendingAction === `kick:${m.username}`}
                            className="rounded-md border border-border bg-bg-primary px-2.5 py-1 text-xs font-medium text-text-primary transition-colors hover:border-warning hover:text-warning disabled:opacity-50"
                          >
                            Kick
                          </button>
                          <button
                            onClick={() =>
                              setConfirm({ kind: "ban", username: m.username })
                            }
                            disabled={pendingAction === `ban:${m.username}`}
                            className="rounded-md border border-error/40 bg-error/10 px-2.5 py-1 text-xs font-medium text-error transition-colors hover:bg-error/20 disabled:opacity-50"
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
            <p className="py-6 text-center text-sm text-text-muted">
              No bans.
            </p>
          ) : (
            <div className="space-y-1">
              {bans.map((username) => (
                <div
                  key={username}
                  className="flex items-center gap-3 rounded-lg px-2 py-2 hover:bg-surface-hover"
                >
                  <div
                    className="flex h-9 w-9 items-center justify-center rounded-lg text-[13px] font-bold text-white opacity-60"
                    style={{ background: stringToGradient(username) }}
                  >
                    {username.charAt(0).toUpperCase()}
                  </div>
                  <span className="flex-1 truncate text-sm font-medium text-text-secondary">
                    {username}
                  </span>
                  <span className="rounded bg-error/15 px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider text-error">
                    Banned
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Footer: leave button for non-owners */}
        {!isOwner && (
          <div className="shrink-0 border-t border-border px-6 py-3">
            <button
              onClick={() => setConfirm({ kind: "leave" })}
              disabled={pendingAction === "leave"}
              className="w-full rounded-lg border border-error/40 bg-error/10 px-4 py-2 text-sm font-semibold text-error transition-colors hover:bg-error/20 disabled:opacity-50"
            >
              Leave Server
            </button>
          </div>
        )}
      </div>

      {/* Confirmation dialog */}
      {confirm && (
        <div
          className="fixed inset-0 z-10 flex items-center justify-center bg-black/50 backdrop-blur-sm"
          onClick={(e) => {
            e.stopPropagation();
            setConfirm(null);
          }}
        >
          <div
            className="w-full max-w-sm rounded-2xl border border-border bg-bg-secondary p-5 shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 text-base font-semibold text-text-bright">
              {confirm.kind === "kick" && `Kick ${confirm.username}?`}
              {confirm.kind === "ban" && `Ban ${confirm.username}?`}
              {confirm.kind === "leave" && "Leave this server?"}
            </h3>
            <p className="mb-4 text-sm text-text-secondary">
              {confirm.kind === "kick" &&
                "They will be disconnected but can rejoin with a valid invite."}
              {confirm.kind === "ban" &&
                "They will be disconnected and prevented from rejoining."}
              {confirm.kind === "leave" &&
                "You will need a new invite to rejoin."}
            </p>
            <div className="flex gap-2">
              <button
                onClick={() => setConfirm(null)}
                className="flex-1 rounded-lg border border-border bg-bg-primary px-4 py-2 text-sm font-semibold text-text-primary transition-colors hover:bg-surface-hover"
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
                className="flex-1 rounded-lg bg-error px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-error/90 disabled:opacity-50"
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
