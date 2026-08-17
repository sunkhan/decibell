import { useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import type { ServerRole } from "../../../types";
import {
  EDITABLE_PERMISSIONS,
  hasBits,
  toggleBit,
  usePermission,
  useHierarchy,
  PERM,
} from "../permissions";
import { ROLE_COLOR_PRESETS, roleColor } from "./helpers";

/// Editor state. `id === null` means "creating".
interface RoleDraft {
  id: number | null;
  name: string;
  color: number;
  permissions: number;
  position: number;
  isDefault: boolean;
}

/// Roles: list, create/edit/delete, reorder. Hierarchy mirrors the
/// server: only roles strictly below your highest are editable (the
/// default role's permissions are editable by any role manager).
export default function RolesTab({ serverId }: { serverId: string }) {
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? []);
  const members = useChatStore((s) => s.membersByServer[serverId] ?? []);
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const { isOwner, level: myLevel } = useHierarchy(serverId);

  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [roleDraft, setRoleDraft] = useState<RoleDraft | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<
    | { roleId: number; roleName: string }
    | null
  >(null);

  const canEditRole = (r: ServerRole) =>
    canManageRoles && (r.isDefault || isOwner || r.position < myLevel);

  const runSaveRole = async () => {
    if (!roleDraft) return;
    setPendingAction("save-role");
    setError(null);
    try {
      if (roleDraft.id === null) {
        await invoke("create_role", {
          serverId,
          name: roleDraft.name.trim(),
          color: roleDraft.color,
          permissions: roleDraft.permissions,
        });
      } else {
        await invoke("update_role", {
          serverId,
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
      await invoke("delete_role", { serverId, roleId });
      if (roleDraft?.id === roleId) setRoleDraft(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
      setConfirmDelete(null);
    }
  };

  /// Reorder by one slot. The server clamps + keeps positions dense.
  const runMoveRole = async (role: ServerRole, delta: number) => {
    setPendingAction(`move-role:${role.id}`);
    setError(null);
    try {
      await invoke("update_role", {
        serverId,
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

  return (
    <div className="flex flex-col gap-1">
      {error && <p className="mb-2 text-[12px] text-error">{error}</p>}

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
                        setConfirmDelete({ roleId: r.id, roleName: r.name })
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

      {/* Delete confirmation */}
      {confirmDelete && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50"
          onClick={(e) => {
            e.stopPropagation();
            setConfirmDelete(null);
          }}
        >
          <div
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              Delete role "{confirmDelete.roleName}"?
            </h3>
            <p className="mb-5 text-[13px] leading-[1.55] text-text-secondary">
              Members holding this role will lose its permissions immediately.
            </p>
            <div className="flex gap-2.5">
              <button
                onClick={() => setConfirmDelete(null)}
                className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
              >
                Cancel
              </button>
              <button
                onClick={() => runDeleteRole(confirmDelete.roleId)}
                disabled={!!pendingAction}
                className="flex-1 rounded-md bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85 disabled:opacity-50"
              >
                {pendingAction ? "Working..." : "Delete"}
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
    <div className="mb-1 ml-6 flex flex-col gap-3 rounded-md border border-border-divider bg-bg-light p-3.5">
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
