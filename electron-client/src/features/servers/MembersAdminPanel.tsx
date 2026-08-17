import { useEffect, useMemo, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { UserAvatar } from "../../components/UserAvatar";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import type { ServerRole } from "../../types";
import {
  EDITABLE_PERMISSIONS,
  PERM,
  hasBits,
  toggleBit,
  usePermission,
  useHierarchy,
} from "./permissions";

type Tab = "members" | "roles" | "bans";

function formatJoined(epoch: number): string {
  if (!epoch) return "—";
  const d = new Date(epoch * 1000);
  return d.toLocaleDateString();
}

/// 0xRRGGBB → css color; 0 (no color) falls back to the muted token.
function roleColor(color: number): string {
  if (!color) return "var(--color-text-muted)";
  return `#${color.toString(16).padStart(6, "0")}`;
}

const ROLE_COLOR_PRESETS = [
  0, 0xe74c3c, 0xe67e22, 0xf1c40f, 0x2ecc71, 0x1abc9c, 0x3498db, 0x9b59b6,
  0xe91e63, 0x95a5a6,
];

/// Editor state for the Roles tab. `id === null` means "creating".
interface RoleDraft {
  id: number | null;
  name: string;
  color: number;
  permissions: number;
  position: number;
  isDefault: boolean;
}

export default function MembersAdminPanel() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  useEscapeToClose(closeModal, activeModal === "members-manage");
  const activeServerId = useChatStore((s) => s.activeServerId);
  const membersByServer = useChatStore((s) => s.membersByServer);
  const bansByServer = useChatStore((s) => s.bansByServer);
  const rolesByServer = useChatStore((s) => s.rolesByServer);
  const currentUser = useAuthStore((s) => s.username);

  const canKick = usePermission(activeServerId, PERM.KICK_MEMBERS);
  const canBan = usePermission(activeServerId, PERM.BAN_MEMBERS);
  const canManageRoles = usePermission(activeServerId, PERM.MANAGE_ROLES);
  const canManageNicknames = usePermission(activeServerId, PERM.MANAGE_NICKNAMES);
  const { isOwner, level: myLevel, levelOf } = useHierarchy(activeServerId);

  const [tab, setTab] = useState<Tab>("members");
  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<
    | { kind: "kick" | "ban" | "leave" | "delete-role"; username?: string; roleId?: number; roleName?: string }
    | null
  >(null);
  const [roleDraft, setRoleDraft] = useState<RoleDraft | null>(null);
  /// Username whose inline role-assignment checkbox list is open.
  const [assignOpen, setAssignOpen] = useState<string | null>(null);
  /// Username whose inline nickname editor is open, and its draft.
  const [nickOpen, setNickOpen] = useState<string | null>(null);
  const [nickDraft, setNickDraft] = useState("");

  const members = activeServerId ? membersByServer[activeServerId] ?? [] : [];
  const bans = activeServerId ? bansByServer[activeServerId] ?? [] : [];
  const roles = activeServerId ? rolesByServer[activeServerId] ?? [] : [];

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

  useEffect(() => {
    if (activeModal === "members-manage" && activeServerId) {
      invoke("list_members", { serverId: activeServerId }).catch((err) =>
        setError(String(err))
      );
      invoke("list_roles", { serverId: activeServerId }).catch(() => {});
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

  const runUnban = async (username: string) => {
    setPendingAction(`unban:${username}`);
    setError(null);
    try {
      await invoke("unban_member", { serverId: activeServerId, username });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
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

  const runSaveRole = async () => {
    if (!roleDraft) return;
    setPendingAction("save-role");
    setError(null);
    try {
      if (roleDraft.id === null) {
        await invoke("create_role", {
          serverId: activeServerId,
          name: roleDraft.name.trim(),
          color: roleDraft.color,
          permissions: roleDraft.permissions,
        });
      } else {
        await invoke("update_role", {
          serverId: activeServerId,
          roleId: roleDraft.id,
          name: roleDraft.name.trim(),
          color: roleDraft.color,
          permissions: roleDraft.permissions,
          position: roleDraft.position,
        });
      }
      setRoleDraft(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  const runDeleteRole = async (roleId: number) => {
    setPendingAction(`delete-role:${roleId}`);
    setError(null);
    try {
      await invoke("delete_role", { serverId: activeServerId, roleId });
      if (roleDraft?.id === roleId) setRoleDraft(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirm(null);
    }
  };

  /// Reorder by one slot. The server clamps + keeps positions dense.
  const runMoveRole = async (role: ServerRole, delta: number) => {
    setPendingAction(`move-role:${role.id}`);
    setError(null);
    try {
      await invoke("update_role", {
        serverId: activeServerId,
        roleId: role.id,
        name: role.name,
        color: role.color,
        permissions: role.permissions,
        position: role.position + delta,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  const runSetNickname = async (username: string) => {
    setPendingAction(`nick:${username}`);
    setError(null);
    try {
      await invoke("set_nickname", {
        serverId: activeServerId,
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
        serverId: activeServerId,
        username,
        roleIds: Array.from(current),
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  /// Whether the local user may edit a given role: MANAGE_ROLES plus
  /// hierarchy (strictly below own highest; owner bypasses). The default
  /// role is always editable permission-wise by role managers.
  const canEditRole = (r: ServerRole) =>
    canManageRoles && (r.isDefault || isOwner || r.position < myLevel);

  const tabButton = (t: Tab, label: string) => (
    <button
      onClick={() => setTab(t)}
      className={`-mb-px border-b-2 px-4 py-2.5 text-[13px] font-medium transition-colors ${
        tab === t
          ? "border-accent text-text-primary"
          : "border-transparent text-text-muted hover:text-text-secondary"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 transition-colors duration-300"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-[520px] animate-[cardIn_0.25s_ease] flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex shrink-0 items-center justify-between px-6 pb-0 pt-5">
          <h2 className="font-display text-[18px] font-semibold text-text-primary">
            Server Members
          </h2>
          <button
            onClick={closeModal}
            className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        {/* Tabs */}
        <div className="flex shrink-0 gap-1 border-b border-border-divider px-6 pt-4">
          {tabButton("members", `Members (${members.length})`)}
          {canManageRoles && tabButton("roles", `Roles (${Math.max(roles.length - 1, 0)})`)}
          {canBan && tabButton("bans", `Banned (${bans.length})`)}
        </div>

        {error && (
          <p className="shrink-0 px-6 pt-3 text-[12px] text-error">{error}</p>
        )}

        {/* List */}
        <div className="flex-1 overflow-y-auto px-5 py-4 scrollbar-thin">
          {tab === "members" && (
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
                          {(isSelf ||
                            (canManageNicknames && !m.isOwner && outranked)) && (
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
              </div>
            )
          )}

          {tab === "roles" && (
            <div className="flex flex-col gap-1">
              {roles.length === 0 && (
                <div className="py-8 text-center text-[13px] text-text-muted">
                  This server doesn't support roles yet.
                </div>
              )}
              {roles.length > 0 && !roleDraft && (
                <button
                  onClick={() =>
                    setRoleDraft({
                      id: null,
                      name: "",
                      color: 0,
                      permissions: 0,
                      position: 1,
                      isDefault: false,
                    })
                  }
                  className="mb-2 w-full rounded-md border border-dashed border-border py-2.5 text-[13px] font-medium text-text-muted transition-colors hover:border-accent hover:text-accent-bright"
                >
                  + New role
                </button>
              )}
              {roles.map((r, idx) => {
                const editable = canEditRole(r);
                const isEditing = roleDraft?.id === r.id;
                // Reordering swaps within the non-default slice only.
                const canMoveUp =
                  editable && !r.isDefault && (isOwner ? idx > 0 : r.position < myLevel - 1);
                const canMoveDown = editable && !r.isDefault && r.position > 1;
                const holderCount = r.isDefault
                  ? members.length
                  : members.filter((m) => m.roleIds.includes(r.id)).length;
                return (
                  <div key={r.id}>
                    <div className="flex items-center gap-3 rounded-md px-3 py-2.5 transition-colors hover:bg-surface-hover">
                      <span
                        className="h-3 w-3 shrink-0 rounded-full"
                        style={{ backgroundColor: roleColor(r.color) }}
                      />
                      <div className="min-w-0 flex-1">
                        <span className="truncate text-[13px] font-medium text-text-primary">
                          {r.isDefault ? "@everyone" : r.name}
                        </span>
                        <span className="ml-2 text-[11px] text-text-muted">
                          {holderCount} member{holderCount === 1 ? "" : "s"}
                        </span>
                      </div>
                      {editable && (
                        <div className="flex shrink-0 items-center gap-1">
                          {!r.isDefault && (
                            <>
                              <button
                                onClick={() => runMoveRole(r, +1)}
                                disabled={!canMoveUp || !!pendingAction}
                                title="Move up"
                                className="flex h-6 w-6 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary disabled:opacity-30"
                              >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><polyline points="18 15 12 9 6 15" /></svg>
                              </button>
                              <button
                                onClick={() => runMoveRole(r, -1)}
                                disabled={!canMoveDown || !!pendingAction}
                                title="Move down"
                                className="flex h-6 w-6 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary disabled:opacity-30"
                              >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><polyline points="6 9 12 15 18 9" /></svg>
                              </button>
                            </>
                          )}
                          <button
                            onClick={() =>
                              setRoleDraft(
                                isEditing
                                  ? null
                                  : {
                                      id: r.id,
                                      name: r.name,
                                      color: r.color,
                                      permissions: r.permissions,
                                      position: r.position,
                                      isDefault: r.isDefault,
                                    },
                              )
                            }
                            className={`rounded-sm px-2.5 py-1.5 text-[11px] font-medium transition-colors ${
                              isEditing
                                ? "bg-accent-soft text-accent-bright"
                                : "bg-bg-light text-text-muted hover:bg-bg-lighter hover:text-text-secondary"
                            }`}
                          >
                            Edit
                          </button>
                          {!r.isDefault && (
                            <button
                              onClick={() =>
                                setConfirm({
                                  kind: "delete-role",
                                  roleId: r.id,
                                  roleName: r.name,
                                })
                              }
                              disabled={!!pendingAction}
                              className="rounded-sm bg-error/10 px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-error/20 hover:text-error disabled:opacity-50"
                            >
                              Delete
                            </button>
                          )}
                        </div>
                      )}
                    </div>
                    {isEditing && roleDraft && (
                      <RoleEditor
                        draft={roleDraft}
                        onChange={setRoleDraft}
                        onSave={runSaveRole}
                        onCancel={() => setRoleDraft(null)}
                        saving={pendingAction === "save-role"}
                      />
                    )}
                  </div>
                );
              })}
              {roleDraft && roleDraft.id === null && (
                <RoleEditor
                  draft={roleDraft}
                  onChange={setRoleDraft}
                  onSave={runSaveRole}
                  onCancel={() => setRoleDraft(null)}
                  saving={pendingAction === "save-role"}
                />
              )}
            </div>
          )}

          {tab === "bans" && (
            bans.length === 0 ? (
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
                    className="flex items-center gap-3 rounded-md px-3 py-2.5 transition-colors hover:bg-surface-hover"
                  >
                    <div className="opacity-50">
                      <UserAvatar username={username} size={36} />
                    </div>
                    <span className="flex-1 truncate text-[13px] font-medium text-text-secondary">
                      {username}
                    </span>
                    {canBan && (
                      <button
                        onClick={() => runUnban(username)}
                        disabled={pendingAction === `unban:${username}`}
                        className="rounded-sm bg-bg-light px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-success/15 hover:text-success disabled:opacity-50"
                      >
                        Unban
                      </button>
                    )}
                    <span className="rounded-sm bg-error/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-error">
                      Banned
                    </span>
                  </div>
                ))}
              </div>
            )
          )}
        </div>

        {/* Footer: leave button for non-owners */}
        {!isOwner && (
          <div className="shrink-0 border-t border-border-divider px-6 py-4">
            <button
              onClick={() => setConfirm({ kind: "leave" })}
              disabled={pendingAction === "leave"}
              className="w-full rounded-md border border-error/20 bg-error/10 py-2.5 text-[13px] font-semibold text-error transition-colors hover:bg-error/20 disabled:opacity-50"
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
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              {confirm.kind === "kick" && `Kick ${confirm.username}?`}
              {confirm.kind === "ban" && `Ban ${confirm.username}?`}
              {confirm.kind === "leave" && "Leave this server?"}
              {confirm.kind === "delete-role" && `Delete role "${confirm.roleName}"?`}
            </h3>
            <p className="mb-5 text-[13px] leading-[1.55] text-text-secondary">
              {confirm.kind === "kick" &&
                "They will be disconnected but can rejoin with a valid invite."}
              {confirm.kind === "ban" &&
                "They will be disconnected and prevented from rejoining."}
              {confirm.kind === "leave" &&
                "You will need a new invite to rejoin."}
              {confirm.kind === "delete-role" &&
                "Members holding this role will lose its permissions immediately."}
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
                  if (confirm.kind === "kick" && confirm.username)
                    runKick(confirm.username);
                  else if (confirm.kind === "ban" && confirm.username)
                    runBan(confirm.username);
                  else if (confirm.kind === "leave") runLeave();
                  else if (confirm.kind === "delete-role" && confirm.roleId)
                    runDeleteRole(confirm.roleId);
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

function RoleEditor({
  draft,
  onChange,
  onSave,
  onCancel,
  saving,
}: {
  draft: RoleDraft;
  onChange: (d: RoleDraft) => void;
  onSave: () => void;
  onCancel: () => void;
  saving: boolean;
}) {
  const canSave = draft.isDefault || draft.name.trim().length > 0;
  return (
    <div className="mb-1 ml-6 mr-3 flex flex-col gap-3 rounded-md border border-border-divider bg-bg-light p-3.5">
      {!draft.isDefault && (
        <>
          <div>
            <label className="mb-1 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
              Name
            </label>
            <input
              value={draft.name}
              onChange={(e) => onChange({ ...draft, name: e.target.value })}
              maxLength={64}
              placeholder="Role name"
              className="w-full rounded-md border border-border bg-bg-dark px-3 py-2 text-[13px] text-text-primary outline-none transition-colors focus:border-accent"
            />
          </div>
          <div>
            <label className="mb-1 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
              Color
            </label>
            <div className="flex flex-wrap gap-1.5">
              {ROLE_COLOR_PRESETS.map((c) => (
                <button
                  key={c}
                  onClick={() => onChange({ ...draft, color: c })}
                  title={c === 0 ? "Default" : `#${c.toString(16).padStart(6, "0")}`}
                  className={`h-6 w-6 rounded-full border-2 transition-transform hover:scale-110 ${
                    draft.color === c ? "border-text-primary" : "border-transparent"
                  }`}
                  style={{
                    backgroundColor:
                      c === 0
                        ? "var(--color-text-muted)"
                        : `#${c.toString(16).padStart(6, "0")}`,
                  }}
                />
              ))}
            </div>
          </div>
        </>
      )}
      <div>
        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
          Permissions
        </label>
        <div className="flex flex-col gap-0.5">
          {EDITABLE_PERMISSIONS.map((p) => {
            const on = hasBits(draft.permissions, p.bit);
            return (
              <label
                key={p.bit}
                className="flex cursor-pointer items-start gap-2.5 rounded-sm px-2 py-1.5 transition-colors hover:bg-surface-hover"
              >
                <input
                  type="checkbox"
                  checked={on}
                  onChange={() =>
                    onChange({
                      ...draft,
                      permissions: toggleBit(draft.permissions, p.bit, !on),
                    })
                  }
                  className="mt-0.5 accent-[var(--color-accent)]"
                />
                <span className="flex-1">
                  <span className="block text-[12px] font-medium text-text-primary">
                    {p.label}
                  </span>
                  <span className="block text-[11px] leading-[1.4] text-text-muted">
                    {p.description}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
      </div>
      <div className="flex gap-2">
        <button
          onClick={onCancel}
          className="flex-1 rounded-md bg-bg-dark py-2 text-[12px] font-medium text-text-secondary transition-colors hover:bg-bg-lighter"
        >
          Cancel
        </button>
        <button
          onClick={onSave}
          disabled={saving || !canSave}
          className="flex-1 rounded-md bg-accent py-2 text-[12px] font-semibold text-white transition-colors hover:bg-accent/85 disabled:opacity-50"
        >
          {saving ? "Saving..." : draft.id === null ? "Create role" : "Save"}
        </button>
      </div>
    </div>
  );
}
