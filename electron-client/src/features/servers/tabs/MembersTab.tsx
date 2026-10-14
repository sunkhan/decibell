import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { clampLine } from "../../../lib/utf8";
import { useChatStore } from "../../../stores/chatStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { useAuthStore } from "../../../stores/authStore";
import { UserAvatar } from "../../../components/UserAvatar";
import { PresenceDot } from "../../../components/StatusDot";
import Select from "../../../components/Select";
import type { MenuAnchor } from "../../../hooks/useMenuPosition";
import type { ServerMember, ServerRole } from "../../../types";
import { PERM, memberLevel, usePermission, useHierarchy } from "../permissions";
import { useMemberSearch } from "./useMemberSearch";
import { roleColor } from "./helpers";
import {
  Icon,
  MenuDivider,
  MenuItem,
  Popover,
  RoleChip,
  SearchField,
  SectionLabel,
  anchorBelow,
  formatDate,
  formatRemaining,
} from "./settingsUi";
import { EmptyState } from "./AuditLogTab";
import { ECHO_WAIT_MS, useEchoSave } from "../saveEcho";
import { NICKNAME_MAX_BYTES } from "../limits";
import { uidOf } from "../../users/userDirectory";

/// Visible role chips per row before the rest fold into "+N".
const MAX_CHIPS = 3;
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
const GRID = "grid grid-cols-[minmax(0,1.15fr)_minmax(0,1.35fr)_7rem_2.25rem] items-center gap-4";

type ModKind = "kick" | "ban" | "timeout";
/// A role toggle waiting for its echo: the set sent, the record it was
/// built on, and when to stop waiting.
type PendingRoles = { roleIds: number[]; base: number[]; until: number };

const sameSet = (a: number[], b: number[]) => a.length === b.length && a.every((id) => b.includes(id));
type Acts = { nick: boolean; roles: boolean; timeout: boolean; endTimeout: boolean; kick: boolean; ban: boolean };

/// Members: a searchable, role-filterable roster. Each row shows who the
/// member is, their roles (assignable in place) and when they joined;
/// every moderation action lives behind the row's ⋯ menu (or a right
/// click), so rows stay quiet. Hierarchy gates mirror the server.
export default function MembersTab({ serverId }: { serverId: string }) {
  // The roster (sidebar data) only feeds the header counts: page 1 of it
  // carries every online member. The table itself is a server search.
  const roster = useChatStore((s) => s.membersByServer[serverId] ?? EMPTY_LIST);
  const totalMembers = useChatStore((s) => s.memberRosterMeta[serverId]?.totalMembers);
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? EMPTY_LIST);
  const currentUser = useAuthStore((s) => s.username);

  const canKick = usePermission(serverId, PERM.KICK_MEMBERS);
  const canBan = usePermission(serverId, PERM.BAN_MEMBERS);
  const canTimeout = usePermission(serverId, PERM.MODERATE_MEMBERS);
  const canManageRoles = usePermission(serverId, PERM.MANAGE_ROLES);
  const canManageNicknames = usePermission(serverId, PERM.MANAGE_NICKNAMES);
  const canChangeOwnNickname = usePermission(serverId, PERM.CHANGE_NICKNAME);
  const { isOwner, level: myLevel, canManageRecord } = useHierarchy(serverId);

  const [query, setQuery] = useState("");
  const [roleFilter, setRoleFilter] = useState<string>("all");
  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<{ kind: ModKind; username: string } | null>(null);
  const [menu, setMenu] = useState<{ username: string; anchor: MenuAnchor } | null>(null);
  const [rolePicker, setRolePicker] = useState<{ username: string; anchor: MenuAnchor } | null>(null);
  const [nickOpen, setNickOpen] = useState<string | null>(null);
  const [nickDraft, setNickDraft] = useState("");
  // Moderation dialog fields (reset when the dialog opens).
  const [reason, setReason] = useState("");
  const [banDurationSec, setBanDurationSec] = useState(0); // 0 = permanent
  const [purgeSec, setPurgeSec] = useState(0); // 0 = none
  const [timeoutSec, setTimeoutSec] = useState(600);
  const openConfirm = (kind: ModKind, username: string) => {
    setReason("");
    setBanDurationSec(0);
    setPurgeSec(0);
    setTimeoutSec(600);
    setConfirm({ kind, username });
  };

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
  const assignable = useMemo(() => new Set(assignableRoles.map((r) => r.id)), [assignableRoles]);

  const search = useMemberSearch(
    serverId,
    query,
    roleFilter === "all" ? 0 : roleFilter === "none" ? -1 : Number(roleFilter),
  );
  const visible = search.members;
  const filtering = !!query.trim() || roleFilter !== "all";
  const onlineCount = useMemo(() => roster.filter((m) => m.isOnline).length, [roster]);

  /// What the local user may do to `m` — one place, read by the row and
  /// its menu alike.
  const actionsFor = (m: ServerMember): Acts => {
    const isSelf = m.username === currentUser;
    // From the record itself: an offline member found by search may not
    // be in the roster at all.
    const outranked = isOwner || memberLevel(roles, m.roleIds) < myLevel;
    const timedOut = (m.timedOutUntil ?? 0) * 1000 > Date.now();
    const mod = !m.isOwner && !isSelf && outranked;
    return {
      nick: (isSelf && canChangeOwnNickname) || (!isSelf && canManageNicknames && canManageRecord(m)),
      roles: canManageRoles && !m.isOwner && assignableRoles.length > 0,
      timeout: mod && canTimeout && !timedOut,
      endTimeout: mod && canTimeout && timedOut,
      kick: mod && canKick,
      ban: mod && canBan,
    };
  };

  // A nickname save keeps the field open until the member's record shows
  // it (saveEcho.ts); a refusal or the wait running out leaves it open.
  const { saving: nickSaving, start: startNick, stop: stopNick } = useEchoSave<{ username: string; nickname: string }>(
    (sent) => visible.find((m) => m.username === sent.username)?.nickname === sent.nickname,
    (sent) => setNickOpen((open) => (open === sent.username ? null : open)),
  );

  // Role toggles settle on the echo too. A toggle shows at once and holds
  // until the member's record (patched by the roster delta) matches, and
  // the next toggle builds on it rather than on the stale record, which
  // used to drop the first of two quick toggles. A refusal or
  // ECHO_WAIT_MS shows the record again.
  const [assigning, setAssigning] = useState<ReadonlyMap<string, PendingRoles>>(new Map());
  const roleIdsOf = (m: ServerMember) => assigning.get(m.username)?.roleIds ?? m.roleIds;
  useEffect(() => {
    if (assigning.size === 0) return;
    const now = Date.now();
    const left = new Map(assigning);
    for (const [username, p] of assigning) {
      const m = visible.find((x) => x.username === username);
      // A fresh record (not the one the toggle was built on) that matches.
      const echoed = !!m && m.roleIds !== p.base && sameSet(m.roleIds, p.roleIds);
      if (echoed || p.until <= now) left.delete(username);
    }
    if (left.size !== assigning.size) {
      setAssigning(left);
      return;
    }
    const next = Math.min(...Array.from(assigning.values(), (p) => p.until));
    const t = setTimeout(
      () =>
        setAssigning((cur) => {
          const live = new Map(Array.from(cur).filter(([, p]) => p.until > Date.now()));
          return live.size === cur.size ? cur : live;
        }),
      next - now,
    );
    return () => clearTimeout(t);
  }, [assigning, visible]);

  // Refusals (useServerEvents toasts them). Always listening, so a quick
  // reply can't beat the listener.
  useEffect(() => {
    const unRole = listen<{ serverId: string; success: boolean; action: string }>("role_action_responded", (event) => {
      const p = event.payload;
      if (p.serverId === serverId && p.action === "assign" && !p.success) setAssigning(new Map());
    });
    const unMod = listen<{ serverId: string; success: boolean; action: string }>("mod_action_responded", (event) => {
      const p = event.payload;
      if (p.serverId === serverId && p.action === "nickname" && !p.success) stopNick();
    });
    return () => {
      unRole.then((fn) => fn());
      unMod.then((fn) => fn());
    };
  }, [serverId, stopNick]);

  const run = async (key: string, fn: () => Promise<unknown>) => {
    setPendingAction(key);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  const runKick = (username: string) =>
    run(`kick:${username}`, () => invoke("kick_member", { serverId, username, uid: uidOf(username), reason: reason.trim() })).then(() =>
      setConfirm(null),
    );
  const runBan = (username: string) =>
    run(`ban:${username}`, () =>
      invoke("ban_member", {
        serverId,
        username,
        uid: uidOf(username),
        reason: reason.trim(),
        expiresAt: banDurationSec > 0 ? Math.floor(Date.now() / 1000) + banDurationSec : 0,
        deleteMessageSeconds: purgeSec,
      }),
    ).then(() => setConfirm(null));
  const runTimeout = (username: string, clear = false) =>
    run(`timeout:${username}`, () =>
      invoke("timeout_member", {
        serverId,
        username,
        uid: uidOf(username),
        until: clear ? 0 : Math.floor(Date.now() / 1000) + timeoutSec,
        reason: clear ? "" : reason.trim(),
      }),
    ).then(() => setConfirm(null));
  const runSetNickname = (username: string) => {
    if (nickSaving) return;
    const nickname = nickDraft.trim();
    setError(null);
    startNick({ username, nickname });
    invoke("set_nickname", { serverId, username, uid: uidOf(username), nickname }).catch((err) => {
      setError(String(err));
      stopNick();
    });
  };
  const runToggleMemberRole = (username: string, roleId: number) => {
    const member = visible.find((m) => m.username === username);
    if (!member) return;
    const ids = new Set(roleIdsOf(member));
    if (ids.has(roleId)) ids.delete(roleId);
    else ids.add(roleId);
    const roleIds = Array.from(ids);
    const drop = () =>
      setAssigning((cur) => {
        const left = new Map(cur);
        left.delete(username);
        return left;
      });
    setAssigning((cur) => new Map(cur).set(username, { roleIds, base: member.roleIds, until: Date.now() + ECHO_WAIT_MS }));
    run(`assign:${username}`, () =>
      invoke("set_member_roles", { serverId, username, uid: uidOf(username), roleIds }).catch((err) => {
        drop();
        throw err;
      }),
    );
  };

  const closeMenu = useCallback(() => setMenu(null), []);
  const closeRolePicker = useCallback(() => setRolePicker(null), []);

  const menuMember = menu ? visible.find((m) => m.username === menu.username) : undefined;
  const pickerMember = rolePicker ? visible.find((m) => m.username === rolePicker.username) : undefined;

  return (
    <div className="flex flex-col">
      {/* Toolbar + column heads stay put while the roster scrolls. */}
      <div className="sticky top-0 z-20 -mx-2 bg-bg-dark px-2">
        <div className="flex items-center gap-2 pb-4">
          <SearchField value={query} onChange={setQuery} placeholder="Search members" className="min-w-0 max-w-80 flex-1" />
          <div className="w-48 shrink-0">
          <Select
            aria-label="Filter by role"
            value={roleFilter}
            onChange={setRoleFilter}
            options={[
              { value: "all", label: "All roles" },
              ...roles
                .filter((r) => !r.isDefault)
                .map((r) => ({
                  value: String(r.id),
                  label: r.name,
                  hint: plural(r.memberCount ?? 0, "member"),
                  icon: <span className="h-2.5 w-2.5 rounded-full" style={{ background: roleColor(r.color) }} />,
                })),
              { value: "none", label: "No roles" },
            ]}
          />
          </div>
          <div className="ml-auto shrink-0 pl-2 text-right font-meta text-meta tabular-nums text-text-muted">
            {filtering ? (
              search.loaded && !search.settling ? (
                <>
                  <span className="text-text-secondary">{search.total.toLocaleString()}</span>{" "}
                  {search.total === 1 ? "match" : "matches"}
                </>
              ) : (
                "Searching…"
              )
            ) : (
              <>
                <span className="text-text-secondary">{(totalMembers ?? search.total).toLocaleString()}</span> members
                <span className="mx-1.5 text-text-faint">·</span>
                <span className="text-success">●</span> {onlineCount} online
              </>
            )}
          </div>
        </div>
        <div className={`${GRID} border-b border-border-divider px-3 pb-2`}>
          <SectionLabel>Member</SectionLabel>
          <SectionLabel>Roles</SectionLabel>
          <SectionLabel>Joined</SectionLabel>
          <span />
        </div>
      </div>

      {(error ?? search.error) && (
        <div className="mt-3 flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
          <Icon name="alert" size={15} />
          <span className="min-w-0 flex-1">{error ?? search.error}</span>
          {error && (
            <button type="button" onClick={() => setError(null)} aria-label="Dismiss" className="rounded-sm p-1 hover:bg-error/10">
              <Icon name="x" size={12} strokeWidth={2.5} />
            </button>
          )}
        </div>
      )}

      {!search.loaded ? (
        <p className="px-3 py-6 text-[13px] text-text-muted">Loading members…</p>
      ) : visible.length === 0 && search.error ? null : visible.length === 0 && !filtering ? (
        <EmptyState icon="users" title="No members yet" body="Invite people and they'll show up here." />
      ) : visible.length === 0 ? (
        <EmptyState
          icon="search"
          title="No matching members"
          body="Try another name, or a different role in the filter."
          action={
            <button
              type="button"
              onClick={() => {
                setQuery("");
                setRoleFilter("all");
              }}
              className="rounded-sm bg-bg-lighter px-4 py-2 text-[13px] font-semibold text-text-primary hover:bg-surface-active"
            >
              Clear filters
            </button>
          }
        />
      ) : (
        <div className="flex flex-col pt-1.5">
          {visible.map((m) => {
            const isSelf = m.username === currentUser;
            const acts = actionsFor(m);
            const anyAction = acts.nick || acts.roles || acts.timeout || acts.endTimeout || acts.kick || acts.ban;
            const memberRoles = roleIdsOf(m)
              .map((id) => rolesById.get(id))
              .filter((r): r is ServerRole => !!r)
              .sort((a, b) => b.position - a.position);
            const topColor = memberRoles.find((r) => r.color)?.color;
            const timedOut = (m.timedOutUntil ?? 0) * 1000 > Date.now();
            const menuOpen = menu?.username === m.username || rolePicker?.username === m.username;
            const openMenuAt = (anchor: MenuAnchor) => {
              setRolePicker(null);
              setMenu({ username: m.username, anchor });
            };
            return (
              <div
                key={m.username}
                onContextMenu={(e) => {
                  if (!anyAction) return;
                  e.preventDefault();
                  openMenuAt({ x: e.clientX, y: e.clientY });
                }}
                className={`group ${GRID} h-14 rounded-md px-3 hover:bg-surface-hover ${menuOpen ? "bg-surface-hover" : ""}`}
              >
                {/* Who */}
                <div className="flex min-w-0 items-center gap-3">
                  <div className="relative shrink-0">
                    <UserAvatar username={m.username} size={32} />
                    <PresenceDot
                      username={m.username}
                      communityOnline={m.isOnline}
                      ring="var(--color-bg-dark)"
                      className="avatar-dot absolute -bottom-px -right-px"
                    />
                  </div>
                  {nickOpen === m.username ? (
                    <form
                      className="flex min-w-0 flex-1 items-center gap-1.5"
                      onSubmit={(e) => {
                        e.preventDefault();
                        runSetNickname(m.username);
                      }}
                    >
                      <input
                        autoFocus
                        value={nickDraft}
                        onChange={(e) => setNickDraft(clampLine(e.target.value, NICKNAME_MAX_BYTES))}
                        onKeyDown={(e) => {
                          if (e.key === "Escape") {
                            e.stopPropagation();
                            setNickOpen(null);
                          }
                        }}
                        placeholder={m.username}
                        className="h-8 min-w-0 flex-1 rounded-sm border border-accent bg-bg-lighter px-2.5 text-[13px] text-text-primary shadow-ring outline-none"
                      />
                      <button
                        type="submit"
                        disabled={nickSaving}
                        aria-label="Save nickname"
                        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-sm bg-accent text-on-accent hover:bg-accent-hover disabled:opacity-50"
                      >
                        <Icon name="check" size={14} strokeWidth={2.5} />
                      </button>
                    </form>
                  ) : (
                    <div className="min-w-0">
                      <div className="flex min-w-0 items-center gap-1.5">
                        <span
                          className="truncate text-[14px] font-medium text-text-primary"
                          style={topColor ? { color: roleColor(topColor) } : undefined}
                        >
                          {m.nickname || m.username}
                        </span>
                        {m.isOwner && (
                          <span title="Server owner" className="flex shrink-0 text-warning">
                            <Icon name="crown" size={13} strokeWidth={2.25} />
                          </span>
                        )}
                        {isSelf && (
                          <span className="shrink-0 rounded-sm bg-accent-soft px-1.5 py-px text-[11px] font-semibold text-accent-bright">
                            You
                          </span>
                        )}
                        {timedOut && (
                          <span
                            title={`Timed out until ${new Date((m.timedOutUntil ?? 0) * 1000).toLocaleString()}`}
                            className="inline-flex shrink-0 items-center gap-1 rounded-sm bg-warning/12 px-1.5 py-px text-[11px] font-semibold tabular-nums text-warning"
                          >
                            <Icon name="clock" size={11} strokeWidth={2.5} />
                            {formatRemaining(m.timedOutUntil ?? 0)}
                          </span>
                        )}
                      </div>
                      {m.nickname && (
                        <div className="truncate font-meta text-meta text-text-muted">{m.username}</div>
                      )}
                    </div>
                  )}
                </div>

                {/* Roles */}
                <div className="flex min-w-0 items-center gap-1.5">
                  {memberRoles.slice(0, MAX_CHIPS).map((r) => (
                    <RoleChip
                      key={r.id}
                      role={r}
                      onRemove={acts.roles && assignable.has(r.id) ? () => runToggleMemberRole(m.username, r.id) : undefined}
                    />
                  ))}
                  {memberRoles.length > MAX_CHIPS && (
                    <span
                      title={memberRoles.slice(MAX_CHIPS).map((r) => r.name).join(", ")}
                      className="inline-flex h-6 shrink-0 items-center rounded-sm border border-border-divider bg-bg-light px-1.5 text-[12px] font-medium tabular-nums text-text-muted"
                    >
                      +{memberRoles.length - MAX_CHIPS}
                    </span>
                  )}
                  {acts.roles && (
                    <button
                      type="button"
                      aria-label={`Edit roles for ${m.nickname || m.username}`}
                      title={memberRoles.length ? "Add role" : undefined}
                      onClick={(e) => {
                        setMenu(null);
                        setRolePicker(
                          rolePicker?.username === m.username ? null : { username: m.username, anchor: anchorBelow(e.currentTarget) },
                        );
                      }}
                      className={`flex h-6 shrink-0 items-center justify-center gap-1 rounded-sm border text-text-muted hover:border-accent hover:text-accent-bright focus-visible:opacity-100 ${
                        memberRoles.length ? "w-6" : "-ml-2 px-2 text-[12px] font-medium"
                      } ${
                        rolePicker?.username === m.username
                          ? "border-accent text-accent-bright"
                          : memberRoles.length
                            ? "border-border opacity-0 group-hover:opacity-100"
                            : "border-transparent group-hover:border-border"
                      }`}
                    >
                      {memberRoles.length ? (
                        <Icon name="plus" size={12} strokeWidth={2.5} />
                      ) : (
                        <>
                          <span className={rolePicker?.username === m.username ? "hidden" : "group-hover:hidden"}>No roles</span>
                          <span className={`items-center gap-1 ${rolePicker?.username === m.username ? "inline-flex" : "hidden group-hover:inline-flex"}`}>
                            <Icon name="plus" size={12} strokeWidth={2.5} />
                            Add role
                          </span>
                        </>
                      )}
                    </button>
                  )}
                  {!memberRoles.length && !acts.roles && <span className="text-[12px] text-text-muted">No roles</span>}
                </div>

                {/* Joined */}
                <div className="truncate font-meta text-meta tabular-nums text-text-secondary" title={new Date(m.joinedAt * 1000).toLocaleString()}>
                  {formatDate(m.joinedAt)}
                </div>

                {/* ⋯ */}
                <div className="flex justify-end">
                  {anyAction && (
                    <button
                      type="button"
                      aria-label={`Actions for ${m.nickname || m.username}`}
                      aria-haspopup="menu"
                      aria-expanded={menu?.username === m.username}
                      onClick={(e) => (menu?.username === m.username ? setMenu(null) : openMenuAt(anchorBelow(e.currentTarget)))}
                      className={`flex h-8 w-8 items-center justify-center rounded-sm text-text-muted hover:bg-surface-active hover:text-text-primary ${
                        menu?.username === m.username ? "bg-surface-active text-text-primary" : ""
                      }`}
                    >
                      <Icon name="more" size={16} />
                    </button>
                  )}
                </div>
              </div>
            );
          })}
          {search.hasMore && (
            <div className="flex justify-center pt-3">
              <button
                type="button"
                onClick={search.loadMore}
                disabled={search.loading}
                className="rounded-sm border border-border px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:cursor-default disabled:opacity-60"
              >
                {search.loading
                  ? "Loading…"
                  : `Show ${Math.max(0, search.total - visible.length).toLocaleString()} more`}
              </button>
            </div>
          )}
        </div>
      )}

      {/* Row menu */}
      {menu && menuMember && (
        <MemberMenu
          member={menuMember}
          anchor={menu.anchor}
          onClose={closeMenu}
          acts={actionsFor(menuMember)}
          onNick={() => {
            setNickDraft(menuMember.nickname);
            setNickOpen(menuMember.username);
            setMenu(null);
          }}
          onRoles={() => {
            setRolePicker({ username: menuMember.username, anchor: menu.anchor });
            setMenu(null);
          }}
          onEndTimeout={() => {
            runTimeout(menuMember.username, true);
            setMenu(null);
          }}
          onMod={(kind) => {
            openConfirm(kind, menuMember.username);
            setMenu(null);
          }}
        />
      )}

      {/* Role picker */}
      {rolePicker && pickerMember && (
        <Popover anchor={rolePicker.anchor} onClose={closeRolePicker} className="w-60 p-1.5">
          <div className="px-2 pb-1.5 pt-1">
            <SectionLabel>Roles</SectionLabel>
          </div>
          {assignableRoles.map((r) => {
            const has = roleIdsOf(pickerMember).includes(r.id);
            return (
              <button
                key={r.id}
                type="button"
                role="menuitemcheckbox"
                aria-checked={has}
                disabled={pendingAction === `assign:${pickerMember.username}`}
                onClick={() => runToggleMemberRole(pickerMember.username, r.id)}
                className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-[13px] font-medium text-text-primary hover:bg-surface-hover"
              >
                <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: roleColor(r.color) }} />
                <span className="min-w-0 flex-1 truncate">{r.name}</span>
                <span
                  className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border ${
                    has ? "border-accent bg-accent text-on-accent" : "border-border bg-bg-lighter"
                  }`}
                >
                  {has && <Icon name="check" size={11} strokeWidth={3} />}
                </span>
              </button>
            );
          })}
          {roles.some((r) => !r.isDefault && !assignable.has(r.id)) && (
            <p className="px-2 pb-1 pt-2 text-[11px] leading-[1.45] text-text-muted">
              Roles at or above your highest role aren’t listed.
            </p>
          )}
        </Popover>
      )}

      {/* Kick / ban / timeout dialog */}
      {confirm && (
        <ModerationDialog
          serverId={serverId}
          kind={confirm.kind}
          username={confirm.username}
          member={visible.find((m) => m.username === confirm.username)}
          reason={reason}
          setReason={setReason}
          banDurationSec={banDurationSec}
          setBanDurationSec={setBanDurationSec}
          purgeSec={purgeSec}
          setPurgeSec={setPurgeSec}
          timeoutSec={timeoutSec}
          setTimeoutSec={setTimeoutSec}
          busy={!!pendingAction}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            if (confirm.kind === "kick") runKick(confirm.username);
            else if (confirm.kind === "ban") runBan(confirm.username);
            else runTimeout(confirm.username);
          }}
        />
      )}
    </div>
  );
}

function MemberMenu({
  member: m,
  anchor,
  onClose,
  acts,
  onNick,
  onRoles,
  onEndTimeout,
  onMod,
}: {
  member: ServerMember;
  anchor: MenuAnchor;
  onClose: () => void;
  acts: Acts;
  onNick: () => void;
  onRoles: () => void;
  onEndTimeout: () => void;
  onMod: (k: ModKind) => void;
}) {
  const name = m.nickname || m.username;
  const anchorRef = useRef<HTMLElement | null>(null);
  return (
    <Popover anchor={anchor} onClose={onClose} ignore={anchorRef} className="w-56 p-1.5">
      <div role="menu" aria-label={`Actions for ${name}`}>
        <div className="flex items-center gap-2.5 px-2 pb-2 pt-1">
          <UserAvatar username={m.username} size={24} />
          <div className="min-w-0">
            <div className="truncate text-[13px] font-semibold text-text-primary">{name}</div>
            {m.nickname && <div className="truncate font-meta text-meta text-text-muted">{m.username}</div>}
          </div>
        </div>
        <MenuDivider />
        {acts.nick && <MenuItem icon="pencil" label={m.nickname ? "Change nickname" : "Set nickname"} onClick={onNick} />}
        {acts.roles && <MenuItem icon="tag" label="Edit roles" onClick={onRoles} />}
        {acts.timeout && <MenuItem icon="clock" label="Time out…" onClick={() => onMod("timeout")} />}
        {acts.endTimeout && <MenuItem icon="clock" label="End timeout" onClick={onEndTimeout} />}
        {(acts.kick || acts.ban) && <MenuDivider />}
        {acts.kick && <MenuItem icon="userX" label={`Kick ${name}…`} onClick={() => onMod("kick")} danger />}
        {acts.ban && <MenuItem icon="ban" label={`Ban ${name}…`} onClick={() => onMod("ban")} danger />}
      </div>
    </Popover>
  );
}

const TIMEOUTS = [
  { value: 60, label: "60 s" },
  { value: 300, label: "5 min" },
  { value: 600, label: "10 min" },
  { value: 3600, label: "1 hour" },
  { value: 86400, label: "1 day" },
  { value: 7 * 86400, label: "1 week" },
];

function ModerationDialog(p: {
  serverId: string;
  kind: ModKind;
  username: string;
  member?: ServerMember;
  reason: string;
  setReason: (v: string) => void;
  banDurationSec: number;
  setBanDurationSec: (v: number) => void;
  purgeSec: number;
  setPurgeSec: (v: number) => void;
  timeoutSec: number;
  setTimeoutSec: (v: number) => void;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const name = p.member?.nickname || p.username;
  const copy = {
    kick: {
      title: `Kick ${name}`,
      body: "They're disconnected now and can come back with a valid invite.",
      cta: "Kick",
      icon: "userX" as const,
    },
    ban: {
      title: `Ban ${name}`,
      body: "They're disconnected and can't rejoin until the ban ends or someone lifts it.",
      cta: "Ban",
      icon: "ban" as const,
    },
    timeout: {
      title: `Time out ${name}`,
      body: "They stay in the server but can't send messages, join voice or stream until it ends.",
      cta: "Time out",
      icon: "clock" as const,
    },
  }[p.kind];
  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50"
      onClick={(e) => {
        e.stopPropagation();
        p.onCancel();
      }}
    >
      <div
        className="w-full max-w-md animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center gap-3">
          <div className="relative shrink-0">
            <UserAvatar username={p.username} size={40} />
            <span
              className={`absolute -bottom-1 -right-1 flex h-5 w-5 items-center justify-center rounded-full border-2 border-bg-dark ${
                p.kind === "timeout" ? "bg-warning text-bg-dark" : "bg-error text-on-error"
              }`}
            >
              <Icon name={copy.icon} size={11} strokeWidth={2.75} />
            </span>
          </div>
          <div className="min-w-0">
            <h3 className="truncate font-display text-[16px] font-semibold text-text-primary">{copy.title}</h3>
            {p.member?.nickname && <div className="truncate font-meta text-meta text-text-muted">{p.username}</div>}
          </div>
        </div>
        <p className="mb-4 text-[13px] leading-[1.55] text-text-secondary">{copy.body}</p>

        {p.kind === "timeout" && (
          <div className="mb-4">
            <SectionLabel className="mb-2">Duration</SectionLabel>
            <div className="grid grid-cols-6 gap-1.5">
              {TIMEOUTS.map((t) => (
                <button
                  key={t.value}
                  type="button"
                  onClick={() => p.setTimeoutSec(t.value)}
                  className={`rounded-sm border py-2 text-[12px] font-semibold ${
                    p.timeoutSec === t.value
                      ? "border-accent bg-accent-soft text-accent-bright"
                      : "border-border text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                  }`}
                >
                  {t.label}
                </button>
              ))}
            </div>
          </div>
        )}
        {p.kind === "ban" && (
          <div className="mb-4 grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-2">
              <SectionLabel>Duration</SectionLabel>
              <Select
                value={p.banDurationSec}
                onChange={p.setBanDurationSec}
                options={[
                  { value: 0, label: "Permanent" },
                  { value: 3600, label: "1 hour" },
                  { value: 86400, label: "1 day" },
                  { value: 7 * 86400, label: "7 days" },
                  { value: 30 * 86400, label: "30 days" },
                ]}
              />
            </label>
            <label className="flex flex-col gap-2">
              <SectionLabel>Delete their messages</SectionLabel>
              <Select
                value={p.purgeSec}
                onChange={p.setPurgeSec}
                options={[
                  { value: 0, label: "Keep all" },
                  { value: 3600, label: "Last hour" },
                  { value: 86400, label: "Last 24 hours" },
                  { value: 7 * 86400, label: "Last 7 days" },
                ]}
              />
            </label>
          </div>
        )}
        <label className="mb-5 flex flex-col gap-2">
          <SectionLabel>Reason</SectionLabel>
          <textarea
            value={p.reason}
            onChange={(e) => p.setReason(e.target.value)}
            maxLength={200}
            rows={2}
            placeholder="Optional. They see it, and so does the audit log."
            className="resize-none rounded-md border border-border bg-bg-lighter px-3 py-2.5 text-[13px] leading-[1.5] text-text-primary outline-none transition-all placeholder:text-text-muted focus:border-accent focus:shadow-ring"
          />
        </label>
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={p.onCancel}
            className="rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={p.onConfirm}
            disabled={p.busy}
            className="rounded-sm bg-error px-4 py-2 text-[13px] font-semibold text-on-error hover:bg-error/85 disabled:opacity-50"
          >
            {p.busy ? "Working…" : copy.cta}
          </button>
        </div>
      </div>
    </div>
  );
}
