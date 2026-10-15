import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { useAuthStore } from "../../../stores/authStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { channelKey } from "../../../lib/channelKey";
import { toast } from "../../../stores/toastStore";
import { UserAvatar } from "../../../components/UserAvatar";
import Switch from "../../../components/Switch";
import type { MenuAnchor } from "../../../hooks/useMenuPosition";
import type { ChannelInfo, ChannelOverwrite, ServerMember, ServerRole } from "../../../types";
import {
  CHANNEL_OVERWRITE_PERMISSIONS,
  PERM,
  PERM_ALL,
  hasBits,
  toggleBit,
  useChannelPermission,
  type PermissionGroup,
} from "../permissions";
import { roleColor } from "../tabs/helpers";
import { Icon, Popover, SearchField, SectionLabel, anchorBelow, type IconName } from "../tabs/settingsUi";
import { useMemberSearch } from "../tabs/useMemberSearch";
import { Notice } from "./ui";
import { EmptyState } from "../tabs/AuditLogTab";
import { nameOf, useUsersStore } from "../../../stores/usersStore";

type TriState = "allow" | "inherit" | "deny";
type Bits = { allow: number; deny: number };
const NO_BITS: Bits = { allow: 0, deny: 0 };

const stateOf = (b: Bits, bit: number): TriState =>
  hasBits(b.deny, bit) ? "deny" : hasBits(b.allow, bit) ? "allow" : "inherit";

/// "role:<id>" | "member:<uid>" → its two halves.
function splitTarget(key: string): ["role" | "member", string] {
  const i = key.indexOf(":");
  return [key.slice(0, i) as "role" | "member", key.slice(i + 1)];
}

/// The draft key of a stored overwrite: members by account, never by name.
function overwriteKey(o: ChannelOverwrite): string {
  return o.targetType === "member" ? `member:${o.targetUid}` : `role:${o.targetId}`;
}

/// Which overwrite bits a channel type shows. A text channel has no use
/// for Connect or Speak, a voice channel none for Send Messages; a
/// category governs both kinds. Hidden bits keep whatever they're set to.
function groupsFor(type: ChannelInfo["type"]): PermissionGroup[] {
  if (type === "text") return ["General", "Text"];
  if (type === "voice") return ["General", "Voice"];
  return ["General", "Text", "Voice"];
}
const GROUP_LABEL: Record<PermissionGroup, string> = {
  General: "Access and management",
  Members: "Members",
  Text: "Text",
  Voice: "Voice",
};

/// Staged permission edits for one channel: overwrites per target (full
/// allow / deny), a category's sync switch (null = unchanged), and
/// targets added in this session that have no overwrite yet. Lives in
/// the modal so switching tabs keeps it.
export function usePermissionDraft(serverId: string, channel: ChannelInfo | undefined) {
  const [pending, setPending] = useState<Record<string, Bits>>({});
  const [syncDraft, setSyncDraft] = useState<boolean | null>(null);
  const [added, setAdded] = useState<string[]>([]);
  const overwrites = useChatStore((s) =>
    channel ? s.overwritesByChannel[channelKey(serverId, channel.id)] ?? EMPTY_LIST : EMPTY_LIST,
  );
  // Staged rows the server now holds drop out of the draft: that's how a
  // save settles (the re-pushed overwrites arrive after the calls return).
  useEffect(() => {
    setPending((p) => {
      let out: Record<string, Bits> | null = null;
      for (const [key, bits] of Object.entries(p)) {
        const ow = overwrites.find((o) => overwriteKey(o) === key);
        if ((ow?.allow ?? 0) === bits.allow && (ow?.deny ?? 0) === bits.deny) {
          out ??= { ...p };
          delete out[key];
        }
      }
      return out ?? p;
    });
    setAdded((a) => {
      const next = a.filter((k) => !overwrites.some((o) => overwriteKey(o) === k));
      return next.length === a.length ? a : next;
    });
  }, [overwrites]);
  useEffect(() => {
    if (syncDraft !== null && syncDraft === !!channel?.categorySync) setSyncDraft(null);
  }, [syncDraft, channel?.categorySync]);
  const dirty =
    Object.keys(pending).length > 0 || (syncDraft !== null && syncDraft !== !!channel?.categorySync);
  const reset = useCallback(() => {
    setPending({});
    setSyncDraft(null);
    setAdded([]);
  }, []);
  /// Overwrites first, then the category switch (so turning sync off
  /// hands its followers the rows as edited). The draft clears as the
  /// server's updates arrive, not here.
  const commit = async () => {
    if (!channel) return;
    for (const [key, bits] of Object.entries(pending)) {
      const [targetType, id] = splitTarget(key);
      const uid = targetType === "member" ? Number(id) : 0;
      await invoke("set_channel_overwrite", {
        serverId,
        channelId: channel.id,
        targetType,
        targetId: targetType === "member" ? "" : id,
        targetUid: uid,
        allow: bits.allow,
        deny: bits.deny,
      });
    }
    if (syncDraft !== null && syncDraft !== !!channel.categorySync) {
      await invoke("set_category_sync", { serverId, channelId: channel.id, enabled: syncDraft });
    }
  };
  return { pending, setPending, syncDraft, setSyncDraft, added, setAdded, dirty, reset, commit };
}
export type PermissionDraft = ReturnType<typeof usePermissionDraft>;

/// Per-channel permission overwrites (permissions v2/v3). The targets with
/// overrides on the left (@everyone always, at the bottom), the selected
/// one's allow / inherit / deny switches on the right. Edits are staged
/// with the rest of the channel settings and go out with the save bar;
/// "Sync now" / "Sync all" are actions and run at once.
///
/// Mirrors the server's guards so the UI doesn't offer what will be
/// refused: only bits the local user holds in this channel are
/// toggleable, and roles at or above the user's level are not offered.
export default function PermissionsTab({
  serverId,
  channel,
  draft,
}: {
  serverId: string;
  channel: ChannelInfo;
  draft: PermissionDraft;
}) {
  const { pending, setPending, syncDraft, setSyncDraft, added, setAdded } = draft;
  const ownUid = useAuthStore((s) => s.uid);
  const owner = useChatStore((s) => s.serverOwner[serverId]);
  const names = useUsersStore((s) => s.names);
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? EMPTY_LIST);
  const members = useChatStore((s) => s.membersByServer[serverId] ?? EMPTY_LIST);
  const overwrites = useChatStore(
    (s) => s.overwritesByChannel[channelKey(serverId, channel.id)] ?? EMPTY_LIST,
  );
  const serverChannels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  const canEdit = useChannelPermission(serverId, channel.id, PERM.MANAGE_ROLES);
  const isCategory = channel.type === "category";

  // Nearest category above the channel in the flat list.
  const parent = useMemo(() => {
    if (isCategory) return undefined;
    let category: ChannelInfo | undefined;
    for (const c of serverChannels) {
      if (c.id === channel.id) return category;
      if (c.type === "category") category = c;
    }
    return undefined;
  }, [serverChannels, channel.id, isCategory]);
  const synced = !!parent && !!channel.permissionsSynced;
  const children = useMemo(() => {
    if (!isCategory) return EMPTY_LIST as ChannelInfo[];
    const out: ChannelInfo[] = [];
    let inside = false;
    for (const c of serverChannels) {
      if (c.type === "category") {
        if (inside) break;
        inside = c.id === channel.id;
      } else if (inside) out.push(c);
    }
    return out;
  }, [serverChannels, channel.id, isCategory]);
  const followers = children.filter((c) => c.permissionsSynced).length;

  const isOwner = !!owner && owner === ownUid;
  const myBits = isOwner ? PERM_ALL : channel.myPermissions ?? 0;
  const me = members.find((m) => m.uid === ownUid);
  const myLevel = isOwner
    ? Number.MAX_SAFE_INTEGER
    : Math.max(0, ...(me?.roleIds ?? []).map((id) => roles.find((r) => r.id === id)?.position ?? 0));

  const everyone = roles.find((r) => r.isDefault);
  const everyoneKey = everyone ? `role:${everyone.id}` : "";

  useEffect(() => {
    invoke("list_channel_overwrites", { serverId, channelId: channel.id }).catch((err) =>
      console.error("list_channel_overwrites:", err),
    );
  }, [serverId, channel.id]);

  const stored = useCallback(
    (key: string): Bits => {
      const ow = overwrites.find((o) => overwriteKey(o) === key);
      return ow ? { allow: ow.allow, deny: ow.deny } : NO_BITS;
    },
    [overwrites],
  );
  const bitsOf = (key: string) => pending[key] ?? stored(key);

  // Targets listed: every stored overwrite, anything staged, anything just
  // added. Roles by rank, then members by name; @everyone apart.
  const targets = useMemo(() => {
    const keys = new Set<string>([
      ...overwrites.map(overwriteKey),
      ...Object.keys(pending),
      ...added,
    ]);
    keys.delete(everyoneKey);
    const rank = (k: string) => {
      const [type, id] = splitTarget(k);
      if (type === "member") return -1;
      return roles.find((r) => String(r.id) === id)?.position ?? 0;
    };
    return Array.from(keys).sort((a, b) => rank(b) - rank(a) || a.localeCompare(b));
  }, [overwrites, pending, added, everyoneKey, roles]);

  const [selected, setSelected] = useState("");
  const target = selected && (selected === everyoneKey || targets.includes(selected)) ? selected : everyoneKey;
  const current = bitsOf(target);
  const [picker, setPicker] = useState<MenuAnchor | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);
  // Names of members picked this session, for one the roster never loaded.
  const pickedNames = useRef(new Map<number, string>());

  const describe = (key: string) => {
    const [type, id] = splitTarget(key);
    if (type === "role") {
      const r = roles.find((x) => String(x.id) === id);
      return { kind: "role" as const, role: r, name: r ? (r.isDefault ? "@everyone" : r.name) : "Deleted role", sub: "" };
    }
    const uid = Number(id);
    const m = members.find((x) => x.uid === uid);
    // A community placeholder (no account) is named only by its rows.
    const username =
      names.get(uid) ||
      m?.username ||
      overwrites.find((o) => o.targetType === "member" && o.targetUid === uid)?.targetId ||
      pickedNames.current.get(uid) ||
      "Unknown user";
    return { kind: "member" as const, uid, username, name: m?.nickname || username, sub: m?.nickname ? username : "" };
  };

  const visible = CHANNEL_OVERWRITE_PERMISSIONS.filter((p) => groupsFor(channel.type).includes(p.group));
  const counts = (b: Bits) => ({
    allow: visible.filter((p) => stateOf(b, p.bit) === "allow").length,
    deny: visible.filter((p) => stateOf(b, p.bit) === "deny").length,
  });

  const stage = (key: string, next: Bits) => {
    const saved = stored(key);
    setPending((p) => {
      const out = { ...p };
      if (next.allow === saved.allow && next.deny === saved.deny) delete out[key];
      else out[key] = next;
      return out;
    });
  };

  const apply = (bit: number, next: TriState) => {
    if (!canEditTarget || !target) return;
    stage(target, {
      allow: toggleBit(current.allow, bit, next === "allow"),
      deny: toggleBit(current.deny, bit, next === "deny"),
    });
  };

  const removeTarget = (key: string) => {
    stage(key, NO_BITS);
    setAdded((a) => a.filter((k) => k !== key));
    if (!overwrites.some((o) => overwriteKey(o) === key)) setSelected(everyoneKey);
  };

  const addTarget = (key: string, name?: string) => {
    const [type, id] = splitTarget(key);
    if (type === "member" && name) pickedNames.current.set(Number(id), name);
    setPicker(null);
    if (key !== everyoneKey && !targets.includes(key)) setAdded((a) => [...a, key]);
    setSelected(key);
  };

  /// A channel syncs with `parent`; a category syncs every channel under it.
  const runSync = async () => {
    if (!canEdit || busy || draft.dirty) return;
    setBusy(true);
    try {
      await invoke("sync_channel_permissions", { serverId, channelId: channel.id, categoryId: parent?.id });
    } catch (err) {
      toast.error("Couldn't sync permissions", String(err));
    } finally {
      setBusy(false);
    }
  };
  const syncTitle = draft.dirty ? "Save or reset your changes first." : undefined;
  const categorySyncShown = syncDraft ?? !!channel.categorySync;
  const plainGroup = isCategory && !categorySyncShown;

  /// Role rows at or above your highest role are the server's to refuse
  /// (everyone is always editable; member rows are any member's).
  const lockedKey = (key: string) => {
    const [type, id] = splitTarget(key);
    if (isOwner || type !== "role") return false;
    const r = roles.find((x) => String(x.id) === id);
    return !!r && !r.isDefault && r.position >= myLevel;
  };

  const t = describe(target);
  const c = counts(current);
  const isEveryone = target === everyoneKey;
  const locked = lockedKey(target);
  const canEditTarget = canEdit && !locked;
  // Clearing a row changes every bit it sets, so you must hold them all.
  const canClear = canEditTarget && hasBits(myBits, current.allow) && hasBits(myBits, current.deny);
  const tileColor = t.kind === "role" && t.role && !t.role.isDefault ? roleColor(t.role.color) : "var(--color-text-secondary)";

  return (
    <div className="flex flex-col gap-6">
      {/* ── Sync with the category ── */}
      {isCategory && (
        <div className="overflow-hidden rounded-lg border border-border-divider bg-bg-light">
          <label className={`flex items-center gap-4 px-4 py-3.5 ${canEdit ? "cursor-pointer hover:bg-bg-lighter" : ""}`}>
            <span className="min-w-0 flex-1">
              <span className="block text-[14px] font-medium text-text-primary">
                Channels follow this category's permissions
              </span>
              <span className="mt-0.5 block text-[12px] leading-[1.55] text-text-muted">
                Off, the category only groups channels. Switching it never changes what a channel
                allows: a channel follows the category once it's synced.
              </span>
            </span>
            <Switch
              checked={categorySyncShown}
              disabled={!canEdit}
              onToggle={() => {
                const next = !categorySyncShown;
                setSyncDraft(next === !!channel.categorySync ? null : next);
              }}
            />
          </label>
          {channel.categorySync && categorySyncShown && children.length > 0 && (
            <SyncStrip
              icon="link"
              text={
                <>
                  <span className="font-semibold text-text-primary tabular-nums">
                    {followers} of {children.length}
                  </span>{" "}
                  channel{children.length === 1 ? "" : "s"} follow these permissions.
                </>
              }
              action={followers < children.length && canEdit ? "Sync all" : undefined}
              onAction={runSync}
              disabled={busy || draft.dirty}
              title={syncTitle}
            />
          )}
        </div>
      )}
      {parent?.categorySync && (
        <div className="overflow-hidden rounded-lg border border-border-divider bg-bg-light">
          <SyncStrip
            icon={synced ? "link" : "linkOff"}
            text={
              synced ? (
                <>
                  Synced with <span className="font-semibold text-text-primary">{parent.name}</span>.
                  Changing anything here gives this channel its own permissions.
                </>
              ) : (
                <>
                  Not synced with <span className="font-semibold text-text-primary">{parent.name}</span>.
                  This channel has its own permissions.
                </>
              )
            }
            action={!synced && canEdit ? "Sync now" : undefined}
            onAction={runSync}
            disabled={busy || draft.dirty}
            title={syncTitle}
          />
        </div>
      )}

      {!canEdit && <Notice icon="lock">You need Manage Roles in this channel to change permissions.</Notice>}

      {plainGroup ? (
        <EmptyState
          icon="folder"
          title="This category only groups channels"
          body="Each channel in it keeps its own permissions. Turn on the switch above to set permissions here that its channels can follow."
        />
      ) : (
        <div className="flex items-start gap-8">
          {/* ── Targets ── */}
          <aside className="sticky top-0 w-56 shrink-0 max-lg:w-48">
            <div className="mb-2 flex h-7 items-center justify-between pl-2">
              <SectionLabel>Roles and members</SectionLabel>
              {canEdit && (
                <button
                  ref={addRef}
                  type="button"
                  onClick={(e) => setPicker(picker ? null : anchorBelow(e.currentTarget.parentElement ?? e.currentTarget))}
                  title="Add a role or member"
                  aria-label="Add a role or member"
                  className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
                >
                  <Icon name="plus" size={15} strokeWidth={2.25} />
                </button>
              )}
            </div>
            <div className="flex flex-col gap-0.5">
              {targets.map((k) => (
                <TargetRow
                  key={k}
                  info={describe(k)}
                  counts={counts(bitsOf(k))}
                  unsaved={!!pending[k]}
                  locked={lockedKey(k)}
                  selected={k === target}
                  onSelect={() => setSelected(k)}
                />
              ))}
              {targets.length === 0 && (
                <p className="px-2 py-1.5 text-[12px] leading-[1.55] text-text-muted">
                  No role or member has its own permissions here yet.
                </p>
              )}
            </div>
            {everyone && (
              <>
                <div className="mx-2 my-2 h-px bg-border-divider" />
                <TargetRow
                  info={describe(everyoneKey)}
                  counts={counts(bitsOf(everyoneKey))}
                  unsaved={!!pending[everyoneKey]}
                  selected={target === everyoneKey}
                  onSelect={() => setSelected(everyoneKey)}
                />
              </>
            )}
            <p className="mt-4 px-2 text-[12px] leading-[1.55] text-text-muted">
              Overrides stack: @everyone first, then roles, then the member. When one role allows and
              another denies, allow wins.
            </p>
          </aside>

          {/* ── Editor ── */}
          <section className="min-w-0 flex-1">
            <header className="flex items-center gap-4 pb-7">
              {t.kind === "member" ? (
                <UserAvatar uid={t.uid} name={t.username} size={48} />
              ) : (
                <span
                  className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg"
                  style={{ color: tileColor, background: `color-mix(in srgb, ${tileColor} 16%, transparent)` }}
                >
                  <Icon name={isEveryone ? "users" : "shield"} size={22} />
                </span>
              )}
              <div className="min-w-0 flex-1">
                <h3 className="truncate font-display text-[18px] font-semibold text-text-primary">
                  {t.name}
                  {t.sub && <span className="ml-2 font-meta text-meta font-normal text-text-muted">{t.sub}</span>}
                </h3>
                <div className="font-meta text-meta tabular-nums text-text-muted">
                  {isEveryone ? "Every member · applies first" : t.kind === "role" ? "Role" : "Member"}
                  {" · "}
                  {c.allow + c.deny === 0
                    ? "follows the server roles"
                    : [c.allow && `${c.allow} allowed`, c.deny && `${c.deny} denied`].filter(Boolean).join(", ")}
                </div>
              </div>
              {canClear && (c.allow + c.deny > 0 || (!isEveryone && added.includes(target))) && (
                <button
                  type="button"
                  onClick={() => (isEveryone ? stage(target, NO_BITS) : removeTarget(target))}
                  className="inline-flex shrink-0 items-center gap-1.5 rounded-sm px-3 py-2 text-[13px] font-semibold text-text-muted hover:bg-surface-hover hover:text-text-primary"
                >
                  <Icon name={isEveryone ? "refresh" : "x"} size={14} />
                  {isEveryone ? "Reset all" : "Remove"}
                </button>
              )}
            </header>

            <div className="flex flex-col gap-7">
              {locked && canEdit && (
                <Notice icon="lock">
                  This role is at or above your highest role. Only the owner or a higher role can
                  change its permissions here.
                </Notice>
              )}
              {groupsFor(channel.type).map((group) => {
                const rows = visible.filter((p) => p.group === group);
                if (!rows.length) return null;
                return (
                  <div key={group}>
                    <SectionLabel className="mb-2.5">{GROUP_LABEL[group]}</SectionLabel>
                    <div className="divide-y divide-border-divider overflow-hidden rounded-lg border border-border-divider bg-bg-light">
                      {rows.map((p) => {
                        const editable = canEditTarget && hasBits(myBits, p.bit);
                        return (
                          <div key={p.bit} className="flex items-center gap-4 px-4 py-3">
                            <span className="min-w-0 flex-1">
                              <span className="block text-[14px] font-medium text-text-primary">{p.label}</span>
                              <span className="mt-0.5 block text-[12px] leading-[1.55] text-text-muted">
                                {p.description}
                              </span>
                            </span>
                            <TriStateControl
                              label={p.label}
                              value={stateOf(current, p.bit)}
                              disabled={!editable}
                              title={
                                editable || !canEditTarget
                                  ? undefined
                                  : "You can only change permissions you hold in this channel."
                              }
                              onChange={(v) => apply(p.bit, v)}
                            />
                          </div>
                        );
                      })}
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        </div>
      )}

      {picker && (
        <AddTargetPopover
          serverId={serverId}
          anchor={picker}
          ignore={addRef}
          roles={roles.filter((r) => !r.isDefault && r.position < myLevel)}
          taken={new Set([...targets, everyoneKey])}
          onClose={() => setPicker(null)}
          onPick={addTarget}
        />
      )}
    </div>
  );
}

function SyncStrip({
  icon,
  text,
  action,
  onAction,
  disabled,
  title,
}: {
  icon: IconName;
  text: React.ReactNode;
  action?: string;
  onAction: () => void;
  disabled: boolean;
  title?: string;
}) {
  return (
    <div className="flex items-center gap-3 border-t border-border-divider px-4 py-3 first:border-t-0">
      <Icon name={icon} size={15} className="text-text-muted" />
      <p className="min-w-0 flex-1 text-[13px] leading-[1.55] text-text-secondary">{text}</p>
      {action && (
        <button
          type="button"
          onClick={onAction}
          disabled={disabled}
          title={title}
          className="shrink-0 rounded-sm border border-border px-3 py-1.5 text-[13px] font-semibold text-text-primary hover:bg-surface-hover disabled:opacity-50"
        >
          {action}
        </button>
      )}
    </div>
  );
}

function TargetRow({
  info,
  counts,
  unsaved,
  locked,
  selected,
  onSelect,
}: {
  info: { kind: "role"; role?: ServerRole; name: string } | { kind: "member"; uid: number; username: string; name: string };
  counts: { allow: number; deny: number };
  unsaved: boolean;
  locked?: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={selected || undefined}
      className={`flex h-9 w-full items-center gap-2.5 rounded-md px-2.5 text-left text-[13px] ${
        selected
          ? "bg-surface-active font-semibold text-text-primary"
          : "font-medium text-text-secondary hover:bg-surface-hover hover:text-text-primary"
      }`}
    >
      {info.kind === "member" ? (
        <UserAvatar uid={info.uid} name={info.username} size={18} />
      ) : (
        <span className="flex h-4.5 w-4.5 shrink-0 items-center justify-center">
          <span
            className="h-2.5 w-2.5 rounded-full"
            style={{ backgroundColor: info.role && !info.role.isDefault ? roleColor(info.role.color) : "var(--color-text-muted)" }}
          />
        </span>
      )}
      <span className="min-w-0 flex-1 truncate">{info.name}</span>
      {locked && <Icon name="lock" size={12} className="text-text-muted" />}
      {unsaved && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-accent" title="Unsaved changes" />}
      <span className="flex shrink-0 items-center gap-1.5 font-meta text-meta font-medium tabular-nums">
        {counts.allow > 0 && (
          <span className="inline-flex items-center gap-0.5 text-success">
            <Icon name="check" size={11} strokeWidth={3} />
            {counts.allow}
          </span>
        )}
        {counts.deny > 0 && (
          <span className="inline-flex items-center gap-0.5 text-error">
            <Icon name="x" size={11} strokeWidth={3} />
            {counts.deny}
          </span>
        )}
      </span>
    </button>
  );
}

const STATES: Array<{ value: TriState; icon: IconName; label: string; on: string }> = [
  { value: "deny", icon: "x", label: "Deny", on: "bg-error/15 text-error ring-1 ring-inset ring-error/40" },
  { value: "inherit", icon: "slash", label: "Inherit", on: "bg-bg-lighter text-text-primary ring-1 ring-inset ring-border" },
  { value: "allow", icon: "check", label: "Allow", on: "bg-success/15 text-success ring-1 ring-inset ring-success/40" },
];

/// Deny / inherit / allow as one segmented control.
function TriStateControl({
  label,
  value,
  disabled,
  title,
  onChange,
}: {
  label: string;
  value: TriState;
  disabled: boolean;
  title?: string;
  onChange: (v: TriState) => void;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      title={title}
      className={`flex shrink-0 gap-0.5 rounded-md border border-border-divider bg-bg-dark p-0.5 ${disabled ? "opacity-50" : ""}`}
    >
      {STATES.map((s) => {
        const on = value === s.value;
        return (
          <button
            key={s.value}
            type="button"
            role="radio"
            aria-checked={on}
            aria-label={`${s.label} ${label}`}
            title={title ? undefined : s.label}
            disabled={disabled}
            onClick={() => onChange(s.value)}
            className={`flex h-7 w-8 items-center justify-center rounded-sm disabled:cursor-not-allowed ${
              on ? s.on : "text-text-muted hover:bg-surface-hover hover:text-text-primary disabled:hover:bg-transparent"
            }`}
          >
            <Icon name={s.icon} size={14} strokeWidth={2.75} />
          </button>
        );
      })}
    </div>
  );
}

/// The + picker: roles below yours, and members found on the server
/// (MEMBER_SEARCH, not just the loaded roster).
function AddTargetPopover({
  serverId,
  anchor,
  ignore,
  roles,
  taken,
  onClose,
  onPick,
}: {
  serverId: string;
  anchor: MenuAnchor;
  ignore: React.RefObject<HTMLElement | null>;
  roles: ServerRole[];
  taken: Set<string>;
  onClose: () => void;
  onPick: (key: string, name?: string) => void;
}) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const found = useMemberSearch(serverId, query, 0);
  const shownRoles = roles
    .filter((r) => !taken.has(`role:${r.id}`) && (!q || r.name.toLowerCase().includes(q)))
    .sort((a, b) => b.position - a.position);
  const shownMembers = found.members.filter((m: ServerMember) => !m.isOwner && !taken.has(`member:${m.uid}`));
  return (
    <Popover anchor={anchor} onClose={onClose} ignore={ignore} className="flex w-72 flex-col p-2">
      <SearchField value={query} onChange={setQuery} placeholder="Find a role or member" autoFocus />
      <div className="mt-2 flex max-h-72 flex-col overflow-y-auto scrollbar-thin">
        {shownRoles.length > 0 && (
          <>
            <SectionLabel className="px-2 pb-1.5 pt-2">Roles</SectionLabel>
            {shownRoles.map((r) => (
              <button
                key={r.id}
                type="button"
                role="menuitem"
                onClick={() => onPick(`role:${r.id}`)}
                className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-[13px] font-medium text-text-primary outline-none hover:bg-surface-hover focus-visible:bg-surface-hover"
              >
                <span className="flex h-6 w-6 shrink-0 items-center justify-center">
                  <span className="h-2.5 w-2.5 rounded-full" style={{ backgroundColor: roleColor(r.color) }} />
                </span>
                <span className="min-w-0 flex-1 truncate">{r.name}</span>
              </button>
            ))}
          </>
        )}
        {shownMembers.length > 0 && (
          <>
            <SectionLabel className="px-2 pb-1.5 pt-3">Members</SectionLabel>
            {shownMembers.map((m) => (
              <button
                key={m.uid}
                type="button"
                role="menuitem"
                onClick={() => onPick(`member:${m.uid}`, nameOf(m.uid, m.username))}
                className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left outline-none hover:bg-surface-hover focus-visible:bg-surface-hover"
              >
                <UserAvatar uid={m.uid} name={nameOf(m.uid, m.username)} size={24} />
                <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-text-primary">
                  {m.nickname || nameOf(m.uid, m.username)}
                  {m.nickname && <span className="ml-1.5 font-meta text-meta font-normal text-text-muted">{nameOf(m.uid, m.username)}</span>}
                </span>
              </button>
            ))}
          </>
        )}
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
        {found.loaded && shownRoles.length === 0 && shownMembers.length === 0 && !found.hasMore && (
          <p className="px-2 py-4 text-center text-[12px] text-text-muted">
            {q ? "No role or member matches." : "Every role and member already has a row."}
          </p>
        )}
      </div>
    </Popover>
  );
}
