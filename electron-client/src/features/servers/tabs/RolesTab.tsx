import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { clampLine } from "../../../lib/utf8";
import { useChatStore } from "../../../stores/chatStore";
import { useAuthStore } from "../../../stores/authStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { UserAvatar } from "../../../components/UserAvatar";
import { PresenceDot } from "../../../components/StatusDot";
import Switch from "../../../components/Switch";
import ConfirmModal from "../../../components/ConfirmModal";
import type { MenuAnchor } from "../../../hooks/useMenuPosition";
import type { ServerMember, ServerRole } from "../../../types";
import {
  EDITABLE_PERMISSIONS,
  PERMISSION_GROUPS,
  hasBits,
  toggleBit,
  usePermission,
  useHierarchy,
  PERM,
} from "../permissions";
import { CustomColorSwatch, onSwatch } from "./ColorPicker";
import { ROLE_COLOR_PRESETS, roleColor } from "./helpers";
import { Icon, Popover, RoleChip, SaveBar, SearchField, SectionLabel, anchorBelow } from "./settingsUi";
import { EmptyState } from "./AuditLogTab";
import { useMemberSearch } from "./useMemberSearch";
import { useEchoSave } from "../saveEcho";
import { useLeaveGuard } from "../leaveGuard";
import { ROLE_NAME_MAX_BYTES } from "../limits";
import { ownUid, uidOf } from "../../users/userDirectory";

/// The editable fields of a role. A new role is a draft with id null.
interface RoleFields {
  name: string;
  color: number;
  permissions: number;
  manageEachOther: boolean;
}

type Section = "display" | "permissions" | "members";
const NEW = "new" as const;
const BLANK: RoleFields = { name: "", color: 0, permissions: 0, manageEachOther: false };

const fieldsOf = (r: ServerRole): RoleFields => ({
  name: r.name,
  color: r.color,
  permissions: r.permissions,
  manageEachOther: !!r.manageEachOther,
});

const sameFields = (a: RoleFields, b: RoleFields) =>
  a.name === b.name && a.color === b.color && a.permissions === b.permissions && a.manageEachOther === b.manageEachOther;

/// What a save sent: the role (null while creating one) and the fields as
/// the server will store them.
interface SentRole {
  roleId: number | null;
  fields: RoleFields;
}

const trimmed = (f: RoleFields): RoleFields => ({ ...f, name: f.name.trim() });

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

const permCount = (mask: number) => EDITABLE_PERMISSIONS.filter((p) => hasBits(mask, p.bit)).length;

/// Roles: the hierarchy on the left (drag to reorder, higher outranks
/// lower), the selected role's editor on the right — Display,
/// Permissions, Members — with changes held in a draft until the save
/// bar commits them. Gates mirror the server: only roles strictly below
/// your highest are editable; `everyone`'s permissions by any manager.
export default function RolesTab({ serverId }: { serverId: string }) {
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? EMPTY_LIST);
  const totalMembers = useChatStore((s) => s.memberRosterMeta[serverId]?.totalMembers ?? 0);
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const { isOwner, level: myLevel } = useHierarchy(serverId);

  const ranked = useMemo(
    () => roles.filter((r) => !r.isDefault).sort((a, b) => b.position - a.position),
    [roles],
  );
  const everyone = useMemo(() => roles.find((r) => r.isDefault), [roles]);

  const canEditRole = useCallback(
    (r: ServerRole) => canManageRoles && (r.isDefault || isOwner || r.position < myLevel),
    [canManageRoles, isOwner, myLevel],
  );

  const [selected, setSelected] = useState<number | typeof NEW | null>(null);
  const [edits, setEdits] = useState<RoleFields | null>(null);
  const [section, setSection] = useState<Section>("display");
  const [nudge, setNudge] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [createdId, setCreatedId] = useState<number | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<ServerRole | null>(null);
  const [drag, setDrag] = useState<{ id: number; drop: number | null } | null>(null);

  // First visit: the most senior role you can edit, else the top one.
  useEffect(() => {
    if (selected !== null && (selected === NEW || roles.some((r) => r.id === selected))) return;
    const first = ranked.find(canEditRole) ?? ranked[0] ?? everyone;
    if (first) setSelected(first.id);
  }, [roles, ranked, everyone, selected, canEditRole]);

  // A save settles on the role list the server pushes after it: the
  // draft stays until the list shows what was sent (saveEcho.ts). A new
  // role's id comes back in the create reply, just before the list.
  const { saving, start: startSave, stop: stopSave } = useEchoSave<SentRole>(
    (sent) => {
      if (sent.roleId === null) return createdId !== null && roles.some((r) => r.id === createdId);
      const r = roles.find((x) => x.id === sent.roleId);
      return !!r && sameFields(fieldsOf(r), sent.fields);
    },
    (sent) => {
      // Edits made while the save was out stay as a fresh draft.
      setEdits((e) => (e && !sameFields(trimmed(e), sent.fields) ? e : null));
      if (sent.roleId === null && createdId !== null) {
        setSelected(createdId);
        setCreatedId(null);
      }
    },
  );

  // The reply to a save: a refusal ends the wait (useServerEvents toasts
  // it), a create names the new role. Always listening, so a quick reply
  // can't beat the listener.
  useEffect(() => {
    const un = listen<{ serverId: string; success: boolean; action: string; role?: ServerRole }>(
      "role_action_responded",
      (event) => {
        const p = event.payload;
        if (p.serverId !== serverId || (p.action !== "update" && p.action !== "create")) return;
        if (!p.success) stopSave();
        else if (p.action === "create" && p.role) setCreatedId(Number(p.role.id));
      },
    );
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId, stopSave]);

  const role = selected === NEW ? null : roles.find((r) => r.id === selected) ?? null;
  const isNew = selected === NEW;
  const stored = role ? fieldsOf(role) : BLANK;
  const fields = edits ?? stored;
  const dirty = isNew || (!!edits && !sameFields(edits, stored));
  const editable = isNew || (!!role && canEditRole(role));
  const sections: Section[] = role?.isDefault ? ["permissions"] : isNew ? ["display", "permissions"] : ["display", "permissions", "members"];
  const activeSection = sections.includes(section) ? section : sections[0];

  // Closing the window or changing tab shakes the bar too (leaveGuard.ts).
  // A nudge belongs to the changes it guarded; the next ones start calm.
  const leaveNudge = useLeaveGuard(dirty);
  useEffect(() => {
    if (!dirty) setNudge(0);
  }, [dirty]);

  // Counts come from the server (role-list pushes), so they include
  // offline members the sidebar never loaded.
  const countOf = (r: ServerRole) => (r.isDefault ? totalMembers : r.memberCount ?? 0);

  const change = (patch: Partial<RoleFields>) => setEdits({ ...fields, ...patch });

  /// Switching away from unsaved changes nudges the save bar instead.
  const select = (next: number | typeof NEW) => {
    if (next === selected) return;
    if (dirty) {
      setNudge((n) => n + 1);
      return;
    }
    setEdits(null);
    setError(null);
    setSelected(next);
  };

  const startCreate = () => {
    if (dirty) {
      setNudge((n) => n + 1);
      return;
    }
    setSelected(NEW);
    setEdits({ ...BLANK });
    setSection("display");
    setError(null);
  };

  const reset = () => {
    setError(null);
    if (isNew) {
      setEdits(null);
      setSelected(ranked[0]?.id ?? everyone?.id ?? null);
    } else setEdits(null);
  };

  const save = async () => {
    if (saving || (!isNew && !role)) return;
    const sent = trimmed(fields);
    setError(null);
    setCreatedId(null);
    startSave({ roleId: isNew ? null : role!.id, fields: sent });
    try {
      if (isNew) {
        await invoke("create_role", {
          serverId,
          name: sent.name,
          color: sent.color,
          permissions: sent.permissions,
          manageEachOther: sent.manageEachOther,
        });
      } else {
        await invoke("update_role", {
          serverId,
          roleId: role!.id,
          name: sent.name,
          color: sent.color,
          permissions: sent.permissions,
          position: role!.position,
          manageEachOther: sent.manageEachOther,
        });
      }
    } catch (err) {
      setError(String(err));
      stopSave();
    }
  };

  const runDelete = async (r: ServerRole) => {
    setError(null);
    try {
      await invoke("delete_role", { serverId, roleId: r.id });
      setEdits(null);
      setSelected(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setConfirmDelete(null);
    }
  };

  /// Reorder to an absolute position; the server clamps + keeps positions
  /// dense. Non-owners can't lift a role to or above their own.
  const moveTo = async (r: ServerRole, position: number) => {
    const max = isOwner ? ranked.length : myLevel - 1;
    const pos = Math.max(1, Math.min(max, position));
    if (pos === r.position) return;
    setError(null);
    try {
      await invoke("update_role", {
        serverId,
        roleId: r.id,
        name: r.name,
        color: r.color,
        permissions: r.permissions,
        position: pos,
        manageEachOther: !!r.manageEachOther,
      });
    } catch (err) {
      setError(String(err));
    }
  };

  const onDrop = () => {
    if (!drag || drag.drop === null) return setDrag(null);
    const r = ranked.find((x) => x.id === drag.id);
    const from = ranked.findIndex((x) => x.id === drag.id);
    setDrag(null);
    if (!r || from < 0) return;
    // `drop` is an insertion index into the list as drawn (dragged row
    // included); index 0 = top = position N.
    const to = drag.drop > from ? drag.drop - 1 : drag.drop;
    moveTo(r, ranked.length - to);
  };

  if (roles.length === 0) {
    return <EmptyState icon="shield" title="No roles here" body="This server doesn't support roles yet." />;
  }

  const roleName = isNew ? fields.name.trim() || "New role" : role?.isDefault ? "@everyone" : fields.name || role?.name || "";

  return (
    <div className="flex items-start gap-8">
      {/* ── Hierarchy ── */}
      <aside className="sticky top-0 w-56 shrink-0 max-lg:w-48">
        <div className="mb-2 flex h-7 items-center justify-between pl-2">
          <SectionLabel>
            Roles <span className="ml-1 tabular-nums">{ranked.length}</span>
          </SectionLabel>
          {canManageRoles && (
            <button
              type="button"
              onClick={startCreate}
              title="Create role"
              aria-label="Create role"
              className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
            >
              <Icon name="plus" size={15} strokeWidth={2.25} />
            </button>
          )}
        </div>
        <div className="flex flex-col gap-0.5" onDragOver={(e) => drag && e.preventDefault()} onDrop={onDrop}>
          {isNew && (
            <RoleRow
              name={fields.name.trim() || "New role"}
              color={fields.color}
              meta="Draft"
              selected
              italic={!fields.name.trim()}
              onSelect={() => {}}
            />
          )}
          {ranked.map((r, i) => {
            const canDrag = canEditRole(r);
            return (
              <RoleRow
                key={r.id}
                name={r.id === selected && edits ? edits.name || r.name : r.name}
                color={r.id === selected && edits ? edits.color : r.color}
                meta={String(countOf(r))}
                selected={r.id === selected}
                locked={!canEditRole(r)}
                dragging={drag?.id === r.id}
                dropLine={drag && drag.id !== r.id ? (drag.drop === i ? "above" : drag.drop === i + 1 && i === ranked.length - 1 ? "below" : null) : null}
                onSelect={() => select(r.id)}
                draggable={canDrag}
                onDragStart={(e) => {
                  e.dataTransfer.effectAllowed = "move";
                  e.dataTransfer.setData("text/plain", String(r.id));
                  setDrag({ id: r.id, drop: null });
                }}
                onDragOver={(e) => {
                  if (!drag) return;
                  e.preventDefault();
                  const box = e.currentTarget.getBoundingClientRect();
                  const drop = e.clientY < box.top + box.height / 2 ? i : i + 1;
                  if (drop !== drag.drop) setDrag({ ...drag, drop });
                }}
                onDragEnd={() => setDrag(null)}
                onKeyDown={(e) => {
                  if (!canDrag || !e.altKey) return;
                  if (e.key === "ArrowUp") {
                    e.preventDefault();
                    moveTo(r, r.position + 1);
                  } else if (e.key === "ArrowDown") {
                    e.preventDefault();
                    moveTo(r, r.position - 1);
                  }
                }}
              />
            );
          })}
        </div>
        {everyone && (
          <>
            <div className="mx-2 my-2 h-px bg-border-divider" />
            <RoleRow
              name="@everyone"
              color={0}
              meta="All"
              selected={everyone.id === selected}
              onSelect={() => select(everyone.id)}
            />
          </>
        )}
        <p className="mt-4 px-2 text-[12px] leading-[1.55] text-text-muted">
          Higher roles outrank lower ones, and members wear the color of their highest role.
          {canManageRoles && " Drag to reorder."}
        </p>
      </aside>

      {/* ── Editor ── */}
      <section className="min-w-0 flex-1">
        <header className="flex items-center gap-4 pb-5">
          <span
            className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg"
            style={{
              color: roleColor(fields.color),
              background: `color-mix(in srgb, ${roleColor(fields.color)} 16%, transparent)`,
            }}
          >
            <Icon name={role?.isDefault ? "users" : "shield"} size={22} />
          </span>
          <div className="min-w-0 flex-1">
            <h3 className={`truncate font-display text-[18px] font-semibold text-text-primary ${isNew && !fields.name.trim() ? "opacity-60" : ""}`}>
              {roleName}
            </h3>
            <div className="font-meta text-meta tabular-nums text-text-muted">
              {role?.isDefault
                ? "Every member · the baseline under all roles"
                : isNew
                  ? "Not created yet"
                  : `${plural(role ? countOf(role) : 0, "member")} · ${plural(permCount(fields.permissions), "permission")}`}
            </div>
          </div>
          {role && !role.isDefault && editable && (
            <button
              type="button"
              onClick={() => setConfirmDelete(role)}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-sm px-3 py-2 text-[13px] font-semibold text-text-muted hover:bg-error/10 hover:text-error"
            >
              <Icon name="trash" size={14} />
              Delete
            </button>
          )}
        </header>

        {!editable && role && (
          <div className="mb-5 flex items-center gap-2.5 rounded-md border border-border-divider bg-bg-light px-3.5 py-3 text-[13px] text-text-secondary">
            <Icon name="lock" size={15} className="text-text-muted" />
            {canManageRoles
              ? "This role is at or above your highest role. Only the owner or a higher role can change it."
              : "You need Manage Roles to change roles."}
          </div>
        )}

        <div role="tablist" className="flex gap-6 border-b border-border-divider">
          {sections.map((s) => (
            <button
              key={s}
              type="button"
              role="tab"
              aria-selected={activeSection === s}
              onClick={() => setSection(s)}
              className={`-mb-px flex items-center gap-1.5 border-b-2 pb-2.5 text-[13px] font-semibold ${
                activeSection === s
                  ? "border-accent text-text-primary"
                  : "border-transparent text-text-muted hover:text-text-secondary"
              }`}
            >
              {s === "display" ? "Display" : s === "permissions" ? "Permissions" : "Members"}
              {s === "permissions" && (
                <span className="rounded-sm bg-surface-active px-1.5 font-meta text-[11px] leading-[18px] tabular-nums text-text-muted">
                  {permCount(fields.permissions)}
                </span>
              )}
              {s === "members" && role && (
                <span className="rounded-sm bg-surface-active px-1.5 font-meta text-[11px] leading-[18px] tabular-nums text-text-muted">
                  {countOf(role)}
                </span>
              )}
            </button>
          ))}
        </div>

        {error && (
          <div className="mt-5 flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
            <Icon name="alert" size={15} />
            {error}
          </div>
        )}

        <div className="pt-6">
          {activeSection === "display" && (
            <DisplaySection serverId={serverId} fields={fields} change={change} editable={editable} autoFocus={isNew} />
          )}
          {activeSection === "permissions" && (
            <PermissionsSection fields={fields} change={change} editable={editable} />
          )}
          {activeSection === "members" && role && (
            <MembersSection serverId={serverId} role={role} canAssign={editable && !role.isDefault} />
          )}
        </div>

        <SaveBar
          show={dirty}
          nudge={nudge + leaveNudge}
          saving={saving}
          onReset={reset}
          onSave={save}
          message={isNew ? "This role doesn't exist until you create it." : undefined}
          nudgeMessage={isNew ? "Create the role or cancel it first." : undefined}
          resetLabel={isNew ? "Cancel" : "Reset"}
          saveLabel={isNew ? "Create role" : "Save changes"}
          saveDisabled={!role?.isDefault && !fields.name.trim()}
        />
      </section>

      <ConfirmModal
        open={!!confirmDelete}
        title={`Delete ${confirmDelete?.name ?? "role"}?`}
        confirmLabel="Delete role"
        onConfirm={() => confirmDelete && runDelete(confirmDelete)}
        onCancel={() => setConfirmDelete(null)}
      >
        Members holding it lose its permissions and color right away. This can't be undone.
      </ConfirmModal>
    </div>
  );
}

function RoleRow({
  name,
  color,
  meta,
  selected,
  locked,
  italic,
  dragging,
  dropLine,
  draggable,
  onSelect,
  onDragStart,
  onDragOver,
  onDragEnd,
  onKeyDown,
}: {
  name: string;
  color: number;
  meta: string;
  selected: boolean;
  locked?: boolean;
  italic?: boolean;
  dragging?: boolean;
  dropLine?: "above" | "below" | null;
  draggable?: boolean;
  onSelect: () => void;
  onDragStart?: (e: React.DragEvent<HTMLButtonElement>) => void;
  onDragOver?: (e: React.DragEvent<HTMLButtonElement>) => void;
  onDragEnd?: () => void;
  onKeyDown?: (e: React.KeyboardEvent<HTMLButtonElement>) => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      draggable={draggable}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onKeyDown={onKeyDown}
      aria-current={selected || undefined}
      title={draggable ? "Drag to reorder (Alt+↑/↓)" : undefined}
      className={`group relative flex h-9 w-full items-center gap-2 rounded-md pl-1 pr-2.5 text-left text-[13px] ${
        selected
          ? "bg-surface-active font-semibold text-text-primary"
          : "font-medium text-text-secondary hover:bg-surface-hover hover:text-text-primary"
      } ${dragging ? "opacity-40" : ""}`}
    >
      {dropLine && (
        <span
          className={`pointer-events-none absolute inset-x-1 h-0.5 rounded-full bg-accent ${dropLine === "above" ? "-top-px" : "-bottom-px"}`}
        />
      )}
      <span
        className={`flex w-4 shrink-0 justify-center text-text-muted ${
          draggable ? "cursor-grab opacity-0 group-hover:opacity-100" : "opacity-0"
        }`}
      >
        <Icon name="grip" size={14} />
      </span>
      <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: roleColor(color) }} />
      <span className={`min-w-0 flex-1 truncate ${italic ? "text-text-muted" : ""}`}>{name}</span>
      {locked && <Icon name="lock" size={12} className="text-text-muted" />}
      <span className="font-meta text-meta tabular-nums text-text-muted">{meta}</span>
    </button>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div>
      <SectionLabel className="mb-2.5">{label}</SectionLabel>
      {children}
      {hint && <p className="mt-2 text-[12px] leading-[1.55] text-text-muted">{hint}</p>}
    </div>
  );
}

function DisplaySection({
  serverId,
  fields,
  change,
  editable,
  autoFocus,
}: {
  serverId: string;
  fields: RoleFields;
  change: (p: Partial<RoleFields>) => void;
  editable: boolean;
  autoFocus: boolean;
}) {
  const me = useAuthStore((s) => s.username) ?? "you";
  const myNick = useChatStore((s) => s.membersByServer[serverId]?.find((m) => m.username === me)?.nickname);
  const previewName = myNick || me;
  return (
    <div className="flex flex-col gap-7">
      <Field label="Role name">
        <input
          value={fields.name}
          autoFocus={autoFocus}
          disabled={!editable}
          onChange={(e) => change({ name: clampLine(e.target.value, ROLE_NAME_MAX_BYTES) })}
          placeholder="e.g. Moderator"
          className="h-10 w-full max-w-sm rounded-md border border-border bg-bg-lighter px-3 text-[13px] text-text-primary outline-none transition-all placeholder:text-text-muted hover:border-accent/40 focus:border-accent focus:shadow-ring disabled:opacity-60"
        />
      </Field>
      <Field label="Color">
        <div className={`flex flex-wrap items-center gap-2.5 ${editable ? "" : "opacity-60"}`}>
          {ROLE_COLOR_PRESETS.map((c) => {
            const on = fields.color === c;
            const hex = `#${c.toString(16).padStart(6, "0")}`;
            return (
              <button
                key={c}
                type="button"
                disabled={!editable}
                onClick={() => change({ color: c })}
                aria-pressed={on}
                aria-label={c === 0 ? "No color" : hex}
                title={c === 0 ? "No color" : hex}
                className={`relative flex h-8 w-8 items-center justify-center rounded-md transition-transform disabled:cursor-not-allowed ${
                  on ? "ring-2 ring-text-primary ring-offset-2 ring-offset-bg-dark" : "hover:scale-110"
                } ${c === 0 ? "border border-border bg-bg-lighter" : ""}`}
                style={c === 0 ? undefined : { backgroundColor: hex }}
              >
                {c === 0 && !on && (
                  <svg viewBox="0 0 32 32" className="absolute inset-0 h-full w-full text-text-muted" aria-hidden="true">
                    <path d="M9 23 23 9" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
                  </svg>
                )}
                {on && (
                  <span style={{ color: c === 0 ? "var(--color-text-primary)" : onSwatch(c) }}>
                    <Icon name="check" size={15} strokeWidth={3} />
                  </span>
                )}
              </button>
            );
          })}
          <span aria-hidden="true" className="mx-0.5 h-5 w-px bg-border-divider" />
          <CustomColorSwatch
            color={fields.color}
            active={fields.color !== 0 && !ROLE_COLOR_PRESETS.includes(fields.color)}
            disabled={!editable}
            onChange={(color) => change({ color })}
          />
        </div>
      </Field>
      <Field label="Preview" hint="How a member whose highest role this is shows up in chat.">
        <div className="flex gap-3 rounded-lg border border-border-divider bg-bg-mid px-4 py-3.5">
          <UserAvatar uid={ownUid()} name={me} size={40} />
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span className="truncate font-channel text-sender font-emphasis" style={{ color: fields.color ? roleColor(fields.color) : "var(--color-text-primary)" }}>
                {previewName}
              </span>
              <span className="font-meta text-meta text-text-muted">
                Today at {new Date().toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}
              </span>
            </div>
            <div className="text-body leading-body text-text-primary">This is how my name looks in chat.</div>
            <div className="mt-2.5 flex">
              <RoleChip
                role={{ id: 0, name: fields.name.trim() || "New role", color: fields.color, position: 0, permissions: 0, isDefault: false }}
              />
            </div>
          </div>
        </div>
      </Field>
    </div>
  );
}

function PermissionsSection({
  fields,
  change,
  editable,
}: {
  fields: RoleFields;
  change: (p: Partial<RoleFields>) => void;
  editable: boolean;
}) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const matches = (p: (typeof EDITABLE_PERMISSIONS)[number]) =>
    !q || p.label.toLowerCase().includes(q) || p.description.toLowerCase().includes(q);
  const admin = hasBits(fields.permissions, PERM.ADMINISTRATOR);
  const anyMatch = EDITABLE_PERMISSIONS.some(matches);
  return (
    <div className="flex flex-col gap-7">
      <div className="flex items-center gap-2">
        <SearchField value={query} onChange={setQuery} placeholder="Search permissions" className="min-w-0 flex-1" />
        {editable && fields.permissions !== 0 && (
          <button
            type="button"
            onClick={() => change({ permissions: 0 })}
            className="shrink-0 rounded-sm px-3 py-2 text-[13px] font-semibold text-text-muted hover:bg-surface-hover hover:text-text-primary"
          >
            Clear all
          </button>
        )}
      </div>

      {admin && !q && (
        <div className="-mt-3 flex gap-2.5 rounded-md border border-warning/30 bg-warning/10 px-3.5 py-3 text-[13px] leading-[1.55] text-text-secondary">
          <Icon name="alert" size={15} className="mt-0.5 text-warning" />
          <span>
            <span className="font-semibold text-text-primary">Administrator is on.</span> This role can do
            everything below, whatever the switches say. Hierarchy still applies.
          </span>
        </div>
      )}

      {!q && (
        <div>
          <SectionLabel className="mb-2.5">Hierarchy</SectionLabel>
          <div className="overflow-hidden rounded-lg border border-border-divider bg-bg-light">
            <PermRow
              label="Members with this role can manage each other"
              description="When it's their highest role, they can change each other's nicknames and use voice moderation on each other. Kicks, bans, timeouts and roles still need a higher role."
              checked={fields.manageEachOther}
              disabled={!editable}
              onToggle={() => change({ manageEachOther: !fields.manageEachOther })}
            />
          </div>
        </div>
      )}

      {PERMISSION_GROUPS.map((group) => {
        const rows = EDITABLE_PERMISSIONS.filter((p) => p.group === group && matches(p));
        if (!rows.length) return null;
        return (
          <div key={group}>
            <SectionLabel className="mb-2.5">{group}</SectionLabel>
            <div className="divide-y divide-border-divider overflow-hidden rounded-lg border border-border-divider bg-bg-light">
              {rows.map((p) => {
                const on = hasBits(fields.permissions, p.bit);
                return (
                  <PermRow
                    key={p.bit}
                    label={p.label}
                    description={p.description}
                    checked={on}
                    disabled={!editable}
                    onToggle={() => change({ permissions: toggleBit(fields.permissions, p.bit, !on) })}
                  />
                );
              })}
            </div>
          </div>
        );
      })}
      {!anyMatch && (
        <p className="py-6 text-center text-[13px] text-text-muted">No permission matches “{query.trim()}”.</p>
      )}
    </div>
  );
}

function PermRow({
  label,
  description,
  checked,
  disabled,
  onToggle,
}: {
  label: string;
  description: string;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  return (
    <label className={`flex items-center gap-4 px-4 py-3.5 ${disabled ? "" : "cursor-pointer hover:bg-bg-lighter"}`}>
      <span className="min-w-0 flex-1">
        <span className="block text-[14px] font-medium text-text-primary">{label}</span>
        <span className="mt-0.5 block text-[12px] leading-[1.55] text-text-muted">{description}</span>
      </span>
      <Switch checked={checked} onToggle={onToggle} disabled={disabled} />
    </label>
  );
}

function MembersSection({
  serverId,
  role,
  canAssign,
}: {
  serverId: string;
  role: ServerRole;
  canAssign: boolean;
}) {
  const [query, setQuery] = useState("");
  const [picker, setPicker] = useState<MenuAnchor | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  const holders = useMemberSearch(serverId, query, role.id);

  const setRole = async (m: ServerMember, on: boolean) => {
    const ids = new Set(m.roleIds);
    if (on) ids.add(role.id);
    else ids.delete(role.id);
    setBusy(m.username);
    try {
      await invoke("set_member_roles", {
        serverId,
        username: m.username,
        uid: m.uid || uidOf(m.username),
        roleIds: Array.from(ids),
      });
    } catch (err) {
      console.error("set_member_roles:", err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-2">
        <SearchField value={query} onChange={setQuery} placeholder="Search members with this role" className="min-w-0 flex-1" />
        {canAssign && (
          <button
            ref={addRef}
            type="button"
            onClick={(e) => setPicker(picker ? null : anchorBelow(e.currentTarget, true))}
            className="inline-flex h-10 shrink-0 items-center gap-2 rounded-sm bg-accent px-4 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
          >
            <Icon name="userPlus" size={15} />
            Add members
          </button>
        )}
      </div>
      {!holders.loaded ? (
        <p className="px-3 py-4 text-[13px] text-text-muted">Loading members…</p>
      ) : holders.error && holders.members.length === 0 ? (
        <div className="flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
          <Icon name="alert" size={15} />
          {holders.error}
        </div>
      ) : holders.members.length === 0 && !query.trim() ? (
        <EmptyState icon="users" title="Nobody has this role yet" body="Add members here, or from the Members tab." />
      ) : (
        <div className="flex flex-col">
          {holders.members.map((m) => (
            <div key={m.username} className="group flex h-12 items-center gap-3 rounded-md px-3 hover:bg-surface-hover">
              <div className="relative shrink-0">
                <UserAvatar uid={m.uid ?? 0} name={m.username} size={28} />
                <PresenceDot
                  uid={m.uid ?? 0}
                  communityOnline={m.isOnline}
                  ring="var(--color-bg-dark)"
                  className="avatar-dot absolute -bottom-px -right-px"
                />
              </div>
              <div className="min-w-0 flex-1">
                <span className="truncate text-[14px] font-medium text-text-primary">{m.nickname || m.username}</span>
                {m.nickname && <span className="ml-2 font-meta text-meta text-text-muted">{m.username}</span>}
              </div>
              {canAssign && (
                <button
                  type="button"
                  disabled={busy === m.username}
                  onClick={() => setRole(m, false)}
                  title={`Remove ${role.name} from ${m.nickname || m.username}`}
                  aria-label={`Remove ${role.name} from ${m.nickname || m.username}`}
                  className="flex h-8 w-8 items-center justify-center rounded-sm text-text-muted opacity-0 hover:bg-error/10 hover:text-error focus-visible:opacity-100 group-hover:opacity-100 disabled:opacity-40"
                >
                  <Icon name="x" size={14} strokeWidth={2.5} />
                </button>
              )}
            </div>
          ))}
          {holders.members.length === 0 && (
            <p className="py-6 text-center text-[13px] text-text-muted">No one with this role matches “{query.trim()}”.</p>
          )}
          {holders.hasMore && (
            <div className="flex justify-center pt-3">
              <button
                type="button"
                onClick={holders.loadMore}
                disabled={holders.loading}
                className="rounded-sm border border-border px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-60"
              >
                {holders.loading ? "Loading…" : `Show ${Math.max(0, holders.total - holders.members.length).toLocaleString()} more`}
              </button>
            </div>
          )}
        </div>
      )}
      {picker && (
        <AddMembersPopover
          serverId={serverId}
          roleId={role.id}
          anchor={picker}
          ignore={addRef}
          onClose={() => setPicker(null)}
          onAdd={async (list) => {
            setPicker(null);
            for (const m of list) await setRole(m, true);
          }}
        />
      )}
    </div>
  );
}

function AddMembersPopover({
  serverId,
  roleId,
  anchor,
  ignore,
  onClose,
  onAdd,
}: {
  serverId: string;
  roleId: number;
  anchor: MenuAnchor;
  ignore: React.RefObject<HTMLElement | null>;
  onClose: () => void;
  onAdd: (members: ServerMember[]) => void;
}) {
  const [query, setQuery] = useState("");
  // Records, not names: a pick stays picked while the search moves on.
  const [picked, setPicked] = useState<Map<string, ServerMember>>(new Map());
  const found = useMemberSearch(serverId, query, 0);
  const shown = found.members.filter((m) => !m.isOwner && !m.roleIds.includes(roleId));
  // Right-aligned under the button: shift the anchor left by our width.
  const right = useMemo(() => ({ x: anchor.x - 288, y: anchor.y }), [anchor]);
  return (
    <Popover anchor={right} onClose={onClose} ignore={ignore} className="flex w-72 flex-col p-2">
      <SearchField value={query} onChange={setQuery} placeholder="Find members" autoFocus />
      <div className="mt-2 flex max-h-64 flex-col overflow-y-auto scrollbar-thin">
        {shown.map((m) => {
          const on = picked.has(m.username);
          return (
            <button
              key={m.username}
              type="button"
              role="menuitemcheckbox"
              aria-checked={on}
              onClick={() => {
                const next = new Map(picked);
                if (on) next.delete(m.username);
                else next.set(m.username, m);
                setPicked(next);
              }}
              className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left hover:bg-surface-hover"
            >
              <UserAvatar uid={m.uid ?? 0} name={m.username} size={24} />
              <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-text-primary">
                {m.nickname || m.username}
                {m.nickname && <span className="ml-1.5 font-meta text-meta font-normal text-text-muted">{m.username}</span>}
              </span>
              <span
                className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border ${
                  on ? "border-accent bg-accent text-on-accent" : "border-border bg-bg-lighter"
                }`}
              >
                {on && <Icon name="check" size={11} strokeWidth={3} />}
              </span>
            </button>
          );
        })}
        {found.hasMore && (
          <button
            type="button"
            onClick={found.loadMore}
            disabled={found.loading}
            className="rounded-md px-2 py-1.5 text-left text-[12px] font-medium text-text-muted hover:bg-surface-hover hover:text-text-primary disabled:opacity-60"
          >
            {found.loading ? "Loading…" : "Show more"}
          </button>
        )}
        {found.loaded && shown.length === 0 && !found.hasMore && (
          <p className="px-2 py-4 text-center text-[12px] text-text-muted">
            {query.trim() ? "No one else matches." : "Everyone here already has it."}
          </p>
        )}
      </div>
      <button
        type="button"
        disabled={picked.size === 0}
        onClick={() => onAdd(Array.from(picked.values()))}
        className="mt-2 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
      >
        {picked.size ? `Add ${picked.size} member${picked.size === 1 ? "" : "s"}` : "Add members"}
      </button>
    </Popover>
  );
}
