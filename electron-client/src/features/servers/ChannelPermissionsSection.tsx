import { useEffect, useMemo, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { EMPTY_LIST } from "../../lib/empty";
import { useAuthStore } from "../../stores/authStore";
import { channelKey } from "../../lib/channelKey";
import { toast } from "../../stores/toastStore";
import type { ChannelInfo, ChannelOverwrite } from "../../types";
import {
  CHANNEL_OVERWRITE_PERMISSIONS,
  PERM,
  PERM_ALL,
  hasBits,
  toggleBit,
  useChannelPermission,
} from "./permissions";

type TriState = "allow" | "inherit" | "deny";

function stateOf(ow: ChannelOverwrite | undefined, bit: number): TriState {
  if (!ow) return "inherit";
  if (hasBits(ow.deny, bit)) return "deny";
  if (hasBits(ow.allow, bit)) return "allow";
  return "inherit";
}

/// Per-channel permission overwrites (permissions v2). Pick a role or a
/// member, then set each channel-scoped bit to allow / inherit / deny.
/// Changes apply immediately; the server re-pushes the channel's
/// overwrites (and everyone's refreshed channel list) on success, and
/// answers denials through channel_action_responded.
///
/// Mirrors the server's guards so the UI doesn't offer what will be
/// refused: only bits the local user holds *in this channel* are
/// toggleable, and roles at or above the user's level are not offered.
///
/// Category sync (permissions v3): a channel under a category either
/// follows the category's overwrites (what's listed is then the
/// category's, and any edit gives the channel its own copy) or has its
/// own, with a "Sync now" to drop them.
export function ChannelPermissionsSection({
  serverId,
  channel,
}: {
  serverId: string;
  channel: ChannelInfo;
}) {
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

  const [kind, id] = target.split(":", 2) as ["role" | "member", string];
  const current = overwrites.find(
    (o) => o.targetType === kind && o.targetId === id,
  );

  const apply = async (bit: number, next: TriState) => {
    if (!canEdit || busy || !target) return;
    let allow = current?.allow ?? 0;
    let deny = current?.deny ?? 0;
    allow = toggleBit(allow, bit, next === "allow");
    deny = toggleBit(deny, bit, next === "deny");
    setBusy(true);
    try {
      await invoke("set_channel_overwrite", {
        serverId,
        channelId: channel.id,
        targetType: kind,
        targetId: id,
        allow,
        deny,
      });
    } catch (err) {
      toast.error("Couldn't update permissions", String(err));
    } finally {
      setBusy(false);
    }
  };

  const runSync = async () => {
    if (!canEdit || busy) return;
    setBusy(true);
    try {
      await invoke("sync_channel_permissions", { serverId, channelId: channel.id });
    } catch (err) {
      toast.error("Couldn't sync permissions", String(err));
    } finally {
      setBusy(false);
    }
  };

  const targetsWithOverwrites = new Set(
    overwrites.map((o) => `${o.targetType}:${o.targetId}`),
  );

  return (
    <div className="mt-6">
      <div className="mb-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
        Permissions
      </div>
      <p className="mb-3 text-[12px] leading-[1.55] text-text-muted">
        {channel.type === "category" ? (
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

      {parent && (
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
              disabled={busy}
              className="shrink-0 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
            >
              Sync now
            </button>
          )}
        </div>
      )}

      <select
        value={target}
        onChange={(e) => setTarget(e.target.value)}
        className="mb-3 w-full appearance-none rounded-md border border-border bg-bg-lighter px-3 py-2.5 pr-9 text-[13px] text-text-primary outline-none transition-all hover:border-text-faint focus:border-accent focus:shadow-ring"
      >
        <optgroup label="Roles">
          {offeredRoles.map((r) => (
            <option key={`role:${r.id}`} value={`role:${r.id}`}>
              {r.isDefault ? "@everyone" : r.name}
              {targetsWithOverwrites.has(`role:${r.id}`) ? " •" : ""}
            </option>
          ))}
        </optgroup>
        <optgroup label="Members">
          {members.map((m) => (
            <option key={`member:${m.username}`} value={`member:${m.username}`}>
              {m.nickname ? `${m.nickname} (${m.username})` : m.username}
              {targetsWithOverwrites.has(`member:${m.username}`) ? " •" : ""}
            </option>
          ))}
        </optgroup>
      </select>

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
      {!canEdit && (
        <p className="mt-2 text-[12px] text-text-muted">
          You need Manage Roles in this channel to change these.
        </p>
      )}
    </div>
  );
}
