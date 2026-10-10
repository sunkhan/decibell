import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { EMPTY_LIST } from "../../lib/empty";
import { useAuthStore } from "../../stores/authStore";
import { channelKey } from "../../lib/channelKey";
import { toast } from "../../stores/toastStore";
import type { ChannelInfo } from "../../types";
import Select from "../../components/Select";
import { UserAvatar } from "../../components/UserAvatar";
import { roleColor } from "./tabs/helpers";
import {
  CHANNEL_OVERWRITE_PERMISSIONS,
  PERM,
  PERM_ALL,
  hasBits,
  toggleBit,
  useChannelPermission,
} from "./permissions";

type TriState = "allow" | "inherit" | "deny";
type Bits = { allow: number; deny: number };

const NO_BITS: Bits = { allow: 0, deny: 0 };

function stateOf(ow: Bits, bit: number): TriState {
  if (hasBits(ow.deny, bit)) return "deny";
  if (hasBits(ow.allow, bit)) return "allow";
  return "inherit";
}

/// "role:<id>" | "member:<username>" → its two halves (usernames never
/// contain ':', but split on the first one only anyway).
function splitTarget(key: string): ["role" | "member", string] {
  const i = key.indexOf(":");
  return [key.slice(0, i) as "role" | "member", key.slice(i + 1)];
}

export interface ChannelPermissionsHandle {
  /// Sends the staged changes: overwrites first, then the category switch
  /// (so turning sync off hands its followers the rows as edited).
  commit: () => Promise<void>;
}

/// Per-channel permission overwrites (permissions v2). Pick a role or a
/// member, then set each channel-scoped bit to allow / inherit / deny.
/// Edits (and a category's sync switch) are staged like the rest of the
/// channel settings: the parent's Save sends them through `commit()`,
/// Cancel drops them, and `onDirtyChange` lights Save up. The server
/// re-pushes the channel's overwrites (and everyone's refreshed channel
/// list) on success and answers denials through channel_action_responded.
/// "Sync now" / "Sync all" are actions, not settings: they run at once
/// (and wait until staged edits are saved or discarded).
///
/// Mirrors the server's guards so the UI doesn't offer what will be
/// refused: only bits the local user holds *in this channel* are
/// toggleable, and roles at or above the user's level are not offered.
///
/// Category sync (permissions v3): a category is a plain group unless its
/// "channels follow this category's permissions" switch is on. Under a
/// syncing category a channel either follows its overwrites (what's
/// listed is then the category's, and any edit gives the channel its own
/// copy) or has its own, with a "Sync now" to drop them. Nothing but an
/// explicit sync changes what a channel allows.
export const ChannelPermissionsSection = forwardRef<
  ChannelPermissionsHandle,
  {
    serverId: string;
    channel: ChannelInfo;
    onDirtyChange?: (dirty: boolean) => void;
  }
>(function ChannelPermissionsSection({ serverId, channel, onDirtyChange }, ref) {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) => s.serverOwner[serverId]);
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? EMPTY_LIST);
  const members = useChatStore((s) => s.membersByServer[serverId] ?? EMPTY_LIST);
  const overwrites = useChatStore(
    (s) => s.overwritesByChannel[channelKey(serverId, channel.id)] ?? EMPTY_LIST,
  );
  const serverChannels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  // Nearest category above the channel in the flat list (none for a
  // category itself or an uncategorized channel).
  const parent = useMemo(() => {
    if (channel.type === "category") return undefined;
    let category: ChannelInfo | undefined;
    for (const c of serverChannels) {
      if (c.id === channel.id) return category;
      if (c.type === "category") category = c;
    }
    return undefined;
  }, [serverChannels, channel.id, channel.type]);
  const synced = !!parent && !!channel.permissionsSynced;
  // A category's channels (as far as we can see them) and how many follow it.
  const children = useMemo(() => {
    if (channel.type !== "category") return EMPTY_LIST as ChannelInfo[];
    const out: ChannelInfo[] = [];
    let inside = false;
    for (const c of serverChannels) {
      if (c.type === "category") {
        if (inside) break;
        inside = c.id === channel.id;
      } else if (inside) {
        out.push(c);
      }
    }
    return out;
  }, [serverChannels, channel.id, channel.type]);
  const followers = children.filter((c) => c.permissionsSynced).length;
  const canEdit = useChannelPermission(serverId, channel.id, PERM.MANAGE_ROLES);
  const canView =
    useChannelPermission(serverId, channel.id, PERM.MANAGE_CHANNELS) || canEdit;

  const isOwner = !!owner && owner === localUsername;
  // Bits the local user may toggle here (escalation guard mirror).
  const myBits = isOwner ? PERM_ALL : channel.myPermissions ?? 0;

  // Hierarchy: roles strictly below mine (everyone is always offered).
  const me = members.find((m) => m.username === localUsername);
  const myLevel = isOwner
    ? Number.MAX_SAFE_INTEGER
    : Math.max(0, ...(me?.roleIds ?? []).map((id) => roles.find((r) => r.id === id)?.position ?? 0));
  const offeredRoles = useMemo(
    () =>
      [...roles]
        .filter((r) => r.isDefault || r.position < myLevel)
        .sort((a, b) => b.position - a.position),
    [roles, myLevel],
  );

  const [target, setTarget] = useState<string>(""); // "role:<id>" | "member:<username>"
  const [busy, setBusy] = useState(false);
  /// Staged overwrites per target (full allow / deny), and the staged
  /// category switch (null = unchanged).
  const [pending, setPending] = useState<Record<string, Bits>>({});
  const [syncDraft, setSyncDraft] = useState<boolean | null>(null);

  const stored = (key: string): Bits => {
    const ow = overwrites.find((o) => `${o.targetType}:${o.targetId}` === key);
    return ow ? { allow: ow.allow, deny: ow.deny } : NO_BITS;
  };
  const categorySyncShown = syncDraft ?? !!channel.categorySync;
  const dirty =
    Object.keys(pending).length > 0 ||
    (syncDraft !== null && syncDraft !== !!channel.categorySync);

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  useImperativeHandle(
    ref,
    () => ({
      commit: async () => {
        for (const [key, bits] of Object.entries(pending)) {
          const [targetType, targetId] = splitTarget(key);
          await invoke("set_channel_overwrite", {
            serverId,
            channelId: channel.id,
            targetType,
            targetId,
            allow: bits.allow,
            deny: bits.deny,
          });
        }
        if (syncDraft !== null && syncDraft !== !!channel.categorySync) {
          await invoke("set_category_sync", { serverId, channelId: channel.id, enabled: syncDraft });
        }
        setPending({});
        setSyncDraft(null);
      },
    }),
    [pending, syncDraft, serverId, channel.id, channel.categorySync],
  );

  useEffect(() => {
    if (!canView) return;
    invoke("list_channel_overwrites", { serverId, channelId: channel.id }).catch(
      (err) => console.error("list_channel_overwrites:", err),
    );
  }, [serverId, channel.id, canView]);

  // Default the picker to @everyone once roles are known.
  useEffect(() => {
    if (target) return;
    const ev = roles.find((r) => r.isDefault);
    if (ev) setTarget(`role:${ev.id}`);
  }, [roles, target]);

  if (!canView) return null;

  const current = pending[target] ?? stored(target);

  /// Stages one bit for the selected target; back at the stored value
  /// the target drops out of the draft.
  const apply = (bit: number, next: TriState) => {
    if (!canEdit || busy || !target) return;
    const allow = toggleBit(current.allow, bit, next === "allow");
    const deny = toggleBit(current.deny, bit, next === "deny");
    const saved = stored(target);
    setPending((p) => {
      const out = { ...p };
      if (allow === saved.allow && deny === saved.deny) delete out[target];
      else out[target] = { allow, deny };
      return out;
    });
  };

  /// A channel syncs with `parent`; a category syncs every channel under it.
  const runSync = async () => {
    if (!canEdit || busy || dirty) return;
    setBusy(true);
    try {
      await invoke("sync_channel_permissions", {
        serverId,
        channelId: channel.id,
        categoryId: parent?.id,
      });
    } catch (err) {
      toast.error("Couldn't sync permissions", String(err));
    } finally {
      setBusy(false);
    }
  };

  const isCategory = channel.type === "category";
  const plainGroup = isCategory && !categorySyncShown;
  const syncTitle = dirty ? "Save or discard your permission changes first." : undefined;

  const targetsWithOverwrites = new Set(
    overwrites.map((o) => `${o.targetType}:${o.targetId}`),
  );
  const optionHint = (key: string): string | undefined => {
    if (pending[key]) return "Unsaved changes";
    const shown = stored(key);
    return shown.allow !== 0 || shown.deny !== 0 || targetsWithOverwrites.has(key)
      ? "Has overrides"
      : undefined;
  };

  return (
    <div className="mt-6">
      <div className="mb-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
        Permissions
      </div>
      {isCategory && (
        <label
          className={`mb-3 flex items-start gap-2.5 rounded-md border border-border-divider bg-bg-light px-3 py-2.5 ${
            canEdit ? "cursor-pointer" : "opacity-60"
          }`}
        >
          <input
            type="checkbox"
            checked={categorySyncShown}
            disabled={!canEdit || busy}
            onChange={() => {
              const next = !categorySyncShown;
              setSyncDraft(next === !!channel.categorySync ? null : next);
            }}
            className="mt-0.5 accent-[var(--color-accent)]"
          />
          <span className="flex-1">
            <span className="block text-[13px] text-text-primary">
              Channels follow this category's permissions
            </span>
            <span className="block text-[11px] leading-[1.4] text-text-muted">
              When off, the category only groups channels. Switching it never changes what
              any channel allows: a channel follows the category once it's synced.
            </span>
          </span>
        </label>
      )}
      {isCategory && channel.categorySync && categorySyncShown && children.length > 0 && (
        <div className="mb-3 flex items-center gap-3 rounded-md border border-border-divider bg-bg-light px-3 py-2.5">
          <p className="min-w-0 flex-1 text-[12px] leading-[1.55] text-text-muted">
            {followers} of {children.length} channel{children.length === 1 ? "" : "s"} follow
            these permissions.
          </p>
          {followers < children.length && canEdit && (
            <button
              type="button"
              onClick={runSync}
              disabled={busy || dirty}
              title={syncTitle}
              className="shrink-0 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
            >
              Sync all
            </button>
          )}
        </div>
      )}
      {plainGroup ? null : (<>
      <p className="mb-3 text-[12px] leading-[1.55] text-text-muted">
        {isCategory ? (
          <>
            Channels synced to this category follow these permissions. Deny{" "}
            <span className="text-text-secondary">View Channel</span> for @everyone to hide
            the category and its synced channels.
          </>
        ) : (
          <>
            Overwrite a role's or member's server permissions for this channel
            only. Deny <span className="text-text-secondary">View Channel</span> for
            @everyone and allow it for a role to make the channel private.
          </>
        )}
      </p>

      {parent?.categorySync && (
        <div className="mb-3 flex items-center gap-3 rounded-md border border-border-divider bg-bg-light px-3 py-2.5">
          <p className="min-w-0 flex-1 text-[12px] leading-[1.55] text-text-muted">
            {synced ? (
              <>
                Synced with <span className="text-text-secondary">{parent.name}</span>. Changing
                anything here gives this channel its own permissions.
              </>
            ) : (
              <>
                Not synced with <span className="text-text-secondary">{parent.name}</span>: this
                channel has its own permissions.
              </>
            )}
          </p>
          {!synced && canEdit && (
            <button
              type="button"
              onClick={runSync}
              disabled={busy || dirty}
              title={syncTitle}
              className="shrink-0 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
            >
              Sync now
            </button>
          )}
        </div>
      )}

      <Select
        className="mb-3"
        aria-label="Overwrite target"
        value={target}
        onChange={setTarget}
        options={[
          {
            label: "Roles",
            options: offeredRoles.map((r) => ({
              value: `role:${r.id}`,
              label: r.isDefault ? "@everyone" : r.name,
              hint: optionHint(`role:${r.id}`),
              icon: <span className="h-2.5 w-2.5 rounded-full" style={{ background: roleColor(r.color) }} />,
            })),
          },
          {
            label: "Members",
            options: members.map((m) => ({
              value: `member:${m.username}`,
              label: m.nickname ? `${m.nickname} (${m.username})` : m.username,
              hint: optionHint(`member:${m.username}`),
              icon: <UserAvatar username={m.username} size={18} />,
            })),
          },
        ]}
      />

      <div className="flex flex-col divide-y divide-border-divider rounded-md border border-border">
        {CHANNEL_OVERWRITE_PERMISSIONS.map((p) => {
          const state = stateOf(current, p.bit);
          const editable = canEdit && hasBits(myBits, p.bit);
          return (
            <div key={p.bit} className="flex items-center gap-3 px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="text-[13px] text-text-primary">{p.label}</div>
                <div className="truncate text-[11px] text-text-muted">
                  {p.description}
                </div>
              </div>
              <div
                className={`flex shrink-0 overflow-hidden rounded-md border border-border ${
                  editable ? "" : "opacity-50"
                }`}
                title={
                  editable
                    ? undefined
                    : "You can only change permissions you hold in this channel."
                }
              >
                {(
                  [
                    ["deny", "✕", "bg-error/10 text-error"],
                    ["inherit", "/", "bg-bg-light text-text-secondary"],
                    ["allow", "✓", "bg-success/15 text-success"],
                  ] as Array<[TriState, string, string]>
                ).map(([value, glyph, activeCls]) => (
                  <button
                    key={value}
                    type="button"
                    disabled={!editable || busy}
                    onClick={() => apply(p.bit, value)}
                    className={`h-7 w-8 text-[12px] font-semibold transition-colors disabled:cursor-not-allowed ${
                      state === value
                        ? activeCls
                        : "text-text-muted hover:bg-surface-hover hover:text-text-primary"
                    }`}
                    aria-label={`${value} ${p.label}`}
                  >
                    {glyph}
                  </button>
                ))}
              </div>
            </div>
          );
        })}
      </div>
      </>)}
      {!canEdit && (
        <p className="mt-2 text-[12px] text-text-muted">
          You need Manage Roles in this channel to change these.
        </p>
      )}
    </div>
  );
});
