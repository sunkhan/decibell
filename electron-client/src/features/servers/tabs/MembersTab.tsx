import { useMemo, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { useAuthStore } from "../../../stores/authStore";
import { UserAvatar } from "../../../components/UserAvatar";
import type { ServerRole } from "../../../types";
import { PERM, usePermission, useHierarchy } from "../permissions";
import { formatJoined, roleColor } from "./helpers";

/// Members: roster with role chips, inline nickname editor, inline role
/// assignment, and hierarchy-aware kick/ban. Moved out of the old
/// MembersAdminPanel into the unified server-settings screen.
export default function MembersTab({ serverId }: { serverId: string }) {
  const members = useChatStore((s) => s.membersByServer[serverId] ?? []);
  const rosterMeta = useChatStore((s) => s.memberRosterMeta[serverId]);
  const setMembersLoadingMore = useChatStore((s) => s.setMembersLoadingMore);
  const loadMore = () => {
    if (!rosterMeta?.hasMore || rosterMeta.loadingMore) return;
    setMembersLoadingMore(serverId, true);
    invoke("list_members", { serverId, after: rosterMeta.nextAfter, limit: 100 }).catch(
      (err) => {
        console.error("list_members:", err);
        setMembersLoadingMore(serverId, false);
      },
    );
  };
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? []);
  const currentUser = useAuthStore((s) => s.username);

  const canKick = usePermission(serverId, PERM.KICK_MEMBERS);
  const canBan = usePermission(serverId, PERM.BAN_MEMBERS);
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const canManageNicknames = usePermission(serverId, PERM.MANAGE_NICKNAMES);
  const { isOwner, level: myLevel, levelOf } = useHierarchy(serverId);

  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<
    | { kind: "kick" | "ban"; username: string }
    | null
  >(null);
  /// Username whose inline role-assignment checkbox list is open.
  const [assignOpen, setAssignOpen] = useState<string | null>(null);
  /// Username whose inline nickname editor is open, and its draft.
  const [nickOpen, setNickOpen] = useState<string | null>(null);
  const [nickDraft, setNickDraft] = useState("");

  const rolesById = useMemo(() => {
    const m = new Map<number, ServerRole>();
    for (const r of roles) m.set(r.id, r);
    return m;
  }, [roles]);

  /// Roles the local user may assign/remove: strictly below their own
  /// highest role (owner bypasses), never the default role. Mirrors the
  /// server's MEMBER_ROLES_UPDATE gate.
  const assignableRoles = useMemo(
    () => roles.filter((r) => !r.isDefault && (isOwner || r.position < myLevel)),
    [roles, isOwner, myLevel],
  );

  const runKick = async (username: string) => {
    setPendingAction(`kick:${username}`);
    setError(null);
    try {
      await invoke("kick_member", { serverId, username, reason: "" });
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
      await invoke("ban_member", { serverId, username, reason: "" });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirm(null);
    }
  };

  const runSetNickname = async (username: string) => {
    setPendingAction(`nick:${username}`);
    setError(null);
    try {
      await invoke("set_nickname", {
        serverId,
        username,
        nickname: nickDraft.trim(),
      });
      setNickOpen(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  const runToggleMemberRole = async (username: string, roleId: number) => {
    const member = members.find((m) => m.username === username);
    if (!member) return;
    const current = new Set(member.roleIds);
    if (current.has(roleId)) current.delete(roleId);
    else current.add(roleId);
    setPendingAction(`assign:${username}`);
    setError(null);
    try {
      await invoke("set_member_roles", {
        serverId,
        username,
        roleIds: Array.from(current),
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  return (
    <div className="flex flex-col">
      {error && <p className="mb-3 text-[12px] text-error">{error}</p>}

      {members.length === 0 ? (
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
            const outranked = isOwner || levelOf(m.username) < myLevel;
            const canModerate =
              (canKick || canBan) && !m.isOwner && !isSelf && outranked;
            const displayName = m.nickname || m.username;
            const memberRoles = m.roleIds
              .map((id) => rolesById.get(id))
              .filter((r): r is ServerRole => !!r);
            const topRole = memberRoles[0];
            return (
              <div key={m.username}>
                <div className="flex items-center gap-3 rounded-md px-3 py-2.5 transition-colors hover:bg-surface-hover">
                  {/* Avatar */}
                  <div className="relative shrink-0">
                    <UserAvatar username={m.username} size={36} />
                    <div
                      className={`absolute -bottom-px -right-px h-[11px] w-[11px] rounded-full border-[2.5px] border-bg-dark ${
                        m.isOnline ? "bg-success" : "bg-text-muted"
                      }`}
                    />
                  </div>

                  {/* Info */}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span
                        className="truncate text-[13px] font-medium text-text-primary"
                        style={
                          topRole && topRole.color
                            ? { color: roleColor(topRole.color) }
                            : undefined
                        }
                      >
                        {displayName}
                      </span>
                      {m.isOwner && (
                        <span className="rounded-sm bg-warning/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-warning">
                          Owner
                        </span>
                      )}
                      {isSelf && (
                        <span className="rounded-sm bg-accent-soft px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-accent-bright">
                          You
                        </span>
                      )}
                    </div>
                    <div className="mt-0.5 flex flex-wrap items-center gap-1 text-[11px] text-text-muted">
                      <span>Joined {formatJoined(m.joinedAt)}</span>
                      {memberRoles.map((r) => (
                        <span
                          key={r.id}
                          className="inline-flex items-center gap-1 rounded-sm border border-border-divider bg-bg-lighter px-1.5 py-px text-[10px] font-medium text-text-secondary"
                        >
                          <span
                            className="h-[7px] w-[7px] rounded-full"
                            style={{ backgroundColor: roleColor(r.color) }}
                          />
                          {r.name}
                        </span>
                      ))}
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="flex shrink-0 gap-1.5">
                    {(isSelf || (canManageNicknames && !m.isOwner && outranked)) && (
                      <button
                        onClick={() => {
                          if (nickOpen === m.username) {
                            setNickOpen(null);
                          } else {
                            setNickOpen(m.username);
                            setNickDraft(m.nickname);
                            setAssignOpen(null);
                          }
                        }}
                        className={`rounded-sm px-2.5 py-1.5 text-[11px] font-medium transition-colors ${
                          nickOpen === m.username
                            ? "bg-accent-soft text-accent-bright"
                            : "bg-bg-light text-text-muted hover:bg-bg-lighter hover:text-text-secondary"
                        }`}
                      >
                        Nick
                      </button>
                    )}
                    {canManageRoles && !m.isOwner && assignableRoles.length > 0 && (
                      <button
                        onClick={() => {
                          setAssignOpen(
                            assignOpen === m.username ? null : m.username,
                          );
                          setNickOpen(null);
                        }}
                        className={`rounded-sm px-2.5 py-1.5 text-[11px] font-medium transition-colors ${
                          assignOpen === m.username
                            ? "bg-accent-soft text-accent-bright"
                            : "bg-bg-light text-text-muted hover:bg-bg-lighter hover:text-text-secondary"
                        }`}
                      >
                        Roles
                      </button>
                    )}
                    {canModerate && canKick && (
                      <button
                        onClick={() =>
                          setConfirm({ kind: "kick", username: m.username })
                        }
                        disabled={pendingAction === `kick:${m.username}`}
                        className="rounded-sm bg-warning/10 px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-warning/20 hover:text-warning disabled:opacity-50"
                      >
                        Kick
                      </button>
                    )}
                    {canModerate && canBan && (
                      <button
                        onClick={() =>
                          setConfirm({ kind: "ban", username: m.username })
                        }
                        disabled={pendingAction === `ban:${m.username}`}
                        className="rounded-sm bg-error/10 px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-error/20 hover:text-error disabled:opacity-50"
                      >
                        Ban
                      </button>
                    )}
                  </div>
                </div>

                {/* Inline nickname editor */}
                {nickOpen === m.username && (
                  <div className="mb-1 ml-[52px] mr-3 flex gap-2 rounded-md border border-border-divider bg-bg-light p-2.5">
                    <input
                      autoFocus
                      value={nickDraft}
                      onChange={(e) => setNickDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") runSetNickname(m.username);
                      }}
                      maxLength={32}
                      placeholder="Nickname (empty to clear)"
                      className="min-w-0 flex-1 rounded-sm border border-border bg-bg-dark px-2.5 py-1.5 text-[12px] text-text-primary outline-none transition-colors focus:border-accent"
                    />
                    <button
                      onClick={() => runSetNickname(m.username)}
                      disabled={pendingAction === `nick:${m.username}`}
                      className="shrink-0 rounded-sm bg-accent px-3 py-1.5 text-[11px] font-semibold text-white transition-colors hover:bg-accent/85 disabled:opacity-50"
                    >
                      Save
                    </button>
                  </div>
                )}

                {/* Inline role assignment */}
                {assignOpen === m.username && (
                  <div className="mb-1 ml-[52px] mr-3 rounded-md border border-border-divider bg-bg-light p-2.5">
                    {assignableRoles.map((r) => {
                      const has = m.roleIds.includes(r.id);
                      return (
                        <label
                          key={r.id}
                          className="flex cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 transition-colors hover:bg-surface-hover"
                        >
                          <input
                            type="checkbox"
                            checked={has}
                            disabled={pendingAction === `assign:${m.username}`}
                            onChange={() => runToggleMemberRole(m.username, r.id)}
                            className="accent-[var(--color-accent)]"
                          />
                          <span
                            className="h-2 w-2 rounded-full"
                            style={{ backgroundColor: roleColor(r.color) }}
                          />
                          <span className="text-[12px] text-text-secondary">
                            {r.name}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
          {rosterMeta?.hasMore && (
            <button
              type="button"
              onClick={loadMore}
              disabled={rosterMeta.loadingMore}
              className="mt-1 w-full rounded-md bg-bg-light py-2 text-[12px] text-text-secondary transition-colors hover:bg-bg-lighter disabled:cursor-default disabled:opacity-60"
            >
              {rosterMeta.loadingMore
                ? "Loading…"
                : `Load more (${Math.max(0, rosterMeta.totalMembers - members.length)} remaining)`}
            </button>
          )}
        </div>
      )}

      {/* Kick/ban confirmation */}
      {confirm && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50"
          onClick={(e) => {
            e.stopPropagation();
            setConfirm(null);
          }}
        >
          <div
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              {confirm.kind === "kick"
                ? `Kick ${confirm.username}?`
                : `Ban ${confirm.username}?`}
            </h3>
            <p className="mb-5 text-[13px] leading-[1.55] text-text-secondary">
              {confirm.kind === "kick"
                ? "They will be disconnected but can rejoin with a valid invite."
                : "They will be disconnected and prevented from rejoining."}
            </p>
            <div className="flex gap-2.5">
              <button
                onClick={() => setConfirm(null)}
                className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
              >
                Cancel
              </button>
              <button
                onClick={() => {
                  if (confirm.kind === "kick") runKick(confirm.username);
                  else runBan(confirm.username);
                }}
                disabled={!!pendingAction}
                className="flex-1 rounded-md bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85 disabled:opacity-50"
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
