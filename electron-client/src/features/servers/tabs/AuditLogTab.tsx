import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { UserAvatar } from "../../../components/UserAvatar";
import Select from "../../../components/Select";
import { useDisplayName } from "../../../hooks/useDisplayName";
import { formatFileSize } from "../../chat/attachmentHelpers";
import type { AuditEntry, AuditLogReceivedPayload, ChannelInfo, ServerRole } from "../../../types";
import { EDITABLE_PERMISSIONS, hasBits } from "../permissions";
import { roleColor } from "./helpers";
import { Icon, SearchField, SectionLabel, type IconName } from "./settingsUi";
import { legacyUid } from "../../users/userDirectory";

type Tone = "danger" | "warning" | "success" | "neutral";
type Category = "moderation" | "members" | "roles" | "channels" | "invites" | "server";

/// Every action the community server writes (src/community/main.cpp
/// add_audit), with its filter group, icon and tone. Tone is the verb's
/// weight, not its subject: destructive = danger, moderation that ends
/// on its own = warning, things that add or restore = success.
const ACTIONS: Record<string, { cat: Category; icon: IconName; tone: Tone }> = {
  kick: { cat: "moderation", icon: "userX", tone: "danger" },
  ban: { cat: "moderation", icon: "ban", tone: "danger" },
  unban: { cat: "moderation", icon: "userCheck", tone: "success" },
  timeout: { cat: "moderation", icon: "clock", tone: "warning" },
  timeout_clear: { cat: "moderation", icon: "clock", tone: "success" },
  message_delete: { cat: "moderation", icon: "messageX", tone: "danger" },
  voice_mod: { cat: "moderation", icon: "micOff", tone: "warning" },
  nickname: { cat: "members", icon: "pencil", tone: "neutral" },
  member_roles: { cat: "members", icon: "tag", tone: "neutral" },
  role_create: { cat: "roles", icon: "shieldPlus", tone: "success" },
  role_update: { cat: "roles", icon: "shield", tone: "neutral" },
  role_delete: { cat: "roles", icon: "shieldX", tone: "danger" },
  channel_create: { cat: "channels", icon: "hash", tone: "success" },
  channel_rename: { cat: "channels", icon: "hash", tone: "neutral" },
  channel_update: { cat: "channels", icon: "sliders", tone: "neutral" },
  channel_delete: { cat: "channels", icon: "trash", tone: "danger" },
  channel_wipe: { cat: "channels", icon: "eraser", tone: "danger" },
  overwrite_set: { cat: "channels", icon: "key", tone: "neutral" },
  overwrite_sync: { cat: "channels", icon: "refresh", tone: "neutral" },
  category_sync: { cat: "channels", icon: "refresh", tone: "neutral" },
  invite_create: { cat: "invites", icon: "link", tone: "success" },
  invite_revoke: { cat: "invites", icon: "linkOff", tone: "danger" },
  server_update: { cat: "server", icon: "gear", tone: "neutral" },
  storage_min_free: { cat: "server", icon: "database", tone: "neutral" },
  ownership_transfer: { cat: "server", icon: "crown", tone: "warning" },
};

const CATEGORY_OPTIONS: Array<{ value: Category | "all"; label: string; icon: IconName }> = [
  { value: "all", label: "All actions", icon: "file" },
  { value: "moderation", label: "Moderation", icon: "ban" },
  { value: "members", label: "Members", icon: "users" },
  { value: "roles", label: "Roles", icon: "shield" },
  { value: "channels", label: "Channels", icon: "hash" },
  { value: "invites", label: "Invites", icon: "link" },
  { value: "server", label: "Server", icon: "gear" },
];

const SEARCH_DEBOUNCE_MS = 250;
/// A community that predates audit search answers with seq 0, which we
/// drop; say so rather than spin forever.
const NO_ANSWER_MS = 8000;

const TONE_TILE: Record<Tone, string> = {
  danger: "bg-error/12 text-error",
  warning: "bg-warning/12 text-warning",
  success: "bg-success/12 text-success",
  neutral: "border border-border-divider bg-bg-light text-text-secondary",
};

/// "2026-10-12 22:40 UTC" (format_utc on the server) → epoch seconds.
function parseUtc(s: string): number | null {
  const m = /(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2}) UTC/.exec(s);
  if (!m) return null;
  return Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5]) / 1000;
}

function shortDateTime(epoch: number): string {
  return new Date(epoch * 1000).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function duration(sec: number): string {
  if (sec >= 86400 * 2 - 60) return `${Math.round(sec / 86400)} days`;
  if (sec >= 86400 - 60) return "1 day";
  if (sec >= 3600 - 30) return `${Math.round(sec / 3600)} h`;
  if (sec >= 60) return `${Math.round(sec / 60)} min`;
  return `${sec} s`;
}

/// The server's "; "-joined detail string, split into its parts.
function detailParts(details: string): string[] {
  return details.split("; ").map((p) => p.trim()).filter(Boolean);
}

/// Bits of a permission mask → their labels, in editor order.
function permLabels(mask: number): string[] {
  if (!mask) return [];
  return EDITABLE_PERMISSIONS.filter((p) => hasBits(mask, p.bit)).map((p) => p.label);
}

function Strong({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span className="font-semibold text-text-primary" title={title}>
      {children}
    </span>
  );
}

function MemberRef({ serverId, username }: { serverId: string; username: string }) {
  const name = useDisplayName(serverId, legacyUid(username), username);
  return <Strong title={name !== username ? username : undefined}>{name}</Strong>;
}

function RoleRef({ role, fallback }: { role?: ServerRole; fallback: string }) {
  if (!role) return <Strong>{fallback}</Strong>;
  return (
    <span className="whitespace-nowrap font-semibold text-text-primary">
      <span
        className="mr-1.5 inline-block h-2 w-2 rounded-full align-middle"
        style={{ backgroundColor: roleColor(role.color) }}
      />
      {role.isDefault ? "@everyone" : role.name}
    </span>
  );
}

function ChannelRef({ channel, fallback }: { channel?: ChannelInfo; fallback: string }) {
  const name = channel?.name ?? fallback;
  const prefix = !channel || channel.type === "text" ? "#" : "";
  return <Strong>{prefix + name}</Strong>;
}

function Chip({
  icon,
  tone,
  children,
  title,
}: {
  icon?: IconName;
  tone?: "success" | "error";
  children: ReactNode;
  title?: string;
}) {
  return (
    <span
      title={title}
      className="inline-flex h-6 max-w-full items-center gap-1.5 rounded-sm border border-border-divider bg-bg-light px-2 text-[12px] text-text-secondary"
    >
      {icon && (
        <Icon
          name={icon}
          size={12}
          strokeWidth={2.5}
          className={tone === "success" ? "text-success" : tone === "error" ? "text-error" : "text-text-muted"}
        />
      )}
      <span className="truncate">{children}</span>
    </span>
  );
}

interface Described {
  icon: IconName;
  tone: Tone;
  /// The sentence after the actor's name.
  text: ReactNode;
  chips: ReactNode[];
  /// A moderator's reason, shown as a quote.
  reason?: string;
}

interface Ctx {
  serverId: string;
  channel: (id: string) => ChannelInfo | undefined;
  roleById: (id: number) => ServerRole | undefined;
  roleByName: (name: string) => ServerRole | undefined;
}

/// One entry → icon, tone, a readable sentence and detail chips. Parses
/// the detail strings the server writes today; anything unrecognised
/// falls through as a plain chip rather than disappearing.
function describe(e: AuditEntry, ctx: Ctx): Described {
  const meta = ACTIONS[e.action] ?? { cat: "server" as Category, icon: "gear" as IconName, tone: "neutral" as Tone };
  const out: Described = { icon: meta.icon, tone: meta.tone, text: null, chips: [] };
  const member = (u: string) => <MemberRef serverId={ctx.serverId} username={u} />;
  const ch = <ChannelRef channel={ctx.channel(e.channelId)} fallback={e.target || e.channelId} />;
  const chIn = e.channelId ? <> in <ChannelRef channel={ctx.channel(e.channelId)} fallback={e.channelId} /></> : null;
  const parts = detailParts(e.details);
  const rest: string[] = [];
  const self = e.actor === e.target;

  // Shared detail grammar: "reason: …", "until <utc>", "N messages purged".
  for (const p of parts) {
    if (p.startsWith("reason: ")) out.reason = p.slice(8);
    else if (p.startsWith("until ") && parseUtc(p.slice(6)) !== null) {
      const until = parseUtc(p.slice(6))!;
      out.chips.push(
        <Chip key="until" icon="clock" title={`Until ${new Date(until * 1000).toLocaleString()}`}>
          {e.action === "timeout" ? `${duration(until - e.timestamp)} · until ${shortDateTime(until)}` : `Until ${shortDateTime(until)}`}
        </Chip>,
      );
    } else if (/^\d+ messages purged$/.test(p)) {
      out.chips.push(
        <Chip key="purge" icon="trash">
          {Number(p.split(" ")[0]).toLocaleString()} messages deleted
        </Chip>,
      );
    } else rest.push(p);
  }

  switch (e.action) {
    case "kick":
      out.text = <>kicked {member(e.target)}</>;
      break;
    case "ban":
      out.text = <>banned {member(e.target)}</>;
      if (!parts.some((p) => p.startsWith("until "))) out.chips.unshift(<Chip key="perm" icon="ban">Permanent</Chip>);
      break;
    case "unban":
      out.text = <>unbanned {member(e.target)}</>;
      break;
    case "timeout":
      out.text = <>timed out {member(e.target)}</>;
      break;
    case "timeout_clear":
      out.text = <>ended the timeout on {member(e.target)}</>;
      break;
    case "message_delete":
      out.text = <>deleted a message by {member(e.target)}{chIn}</>;
      rest.length = 0; // "message <id>" — the id means nothing to a reader
      break;
    case "voice_mod": {
      const verb = rest.shift() ?? "";
      const where = <ChannelRef channel={ctx.channel(e.channelId)} fallback={e.channelId} />;
      if (verb.startsWith("voice_move")) {
        out.icon = "move";
        out.tone = "neutral";
        out.text = <>moved {member(e.target)} to {where}</>;
        if (verb.includes("voice pass"))
          out.chips.push(
            <Chip key="pass" icon="key" title="They couldn't join that channel themselves, so the move granted a one-time voice pass.">
              Voice pass granted
            </Chip>,
          );
      } else if (verb === "voice_disconnect") {
        out.icon = "logout";
        out.text = <>disconnected {member(e.target)} from {where}</>;
      } else {
        const undo = verb.includes("un");
        const deafen = verb.includes("deafen");
        out.icon = deafen ? "headphones" : undo ? "mic" : "micOff";
        out.tone = undo ? "success" : "warning";
        out.text = (
          <>
            {undo ? (deafen ? "undeafened" : "unmuted") : deafen ? "server-deafened" : "server-muted"}{" "}
            {member(e.target)} in {where}
          </>
        );
      }
      break;
    }
    case "nickname": {
      const d = rest.shift() ?? "";
      // The username, not the display name: the nickname is what changed.
      const whose = self ? "their own" : <><Strong>{e.target}</Strong>’s</>;
      out.text =
        d === "cleared" ? (
          <>cleared {whose} nickname</>
        ) : (
          <>
            changed {whose} nickname to <Strong>{d.replace(/^set to /, "")}</Strong>
          </>
        );
      break;
    }
    case "member_roles": {
      out.text = self ? <>changed their own roles</> : <>changed the roles of {member(e.target)}</>;
      const d = rest.shift() ?? "";
      for (const m of d.matchAll(/([+-])(.+?) (?=[+-]|$)/g)) {
        const role = ctx.roleByName(m[2]);
        const add = m[1] === "+";
        out.chips.push(
          <span
            key={m[0]}
            className={`inline-flex h-6 items-center gap-1.5 rounded-sm border border-border-divider bg-bg-light px-2 text-[12px] ${
              add ? "text-text-secondary" : "text-text-muted line-through decoration-text-muted/60"
            }`}
          >
            <Icon name={add ? "plus" : "x"} size={11} strokeWidth={3} className={add ? "text-success" : "text-error"} />
            <span className="h-2 w-2 rounded-full" style={{ backgroundColor: roleColor(role?.color ?? 0) }} />
            {m[2]}
          </span>,
        );
      }
      break;
    }
    case "role_create":
      out.text = <>created the role <RoleRef role={ctx.roleByName(e.target)} fallback={e.target} /></>;
      break;
    case "role_update": {
      const role = e.target === "everyone" ? ctx.roleById(-1) : ctx.roleByName(e.target);
      out.text = <>updated the role <RoleRef role={role} fallback={e.target === "everyone" ? "@everyone" : e.target} /></>;
      for (const p of rest.splice(0)) {
        if (p === "permissions changed") out.chips.push(<Chip key={p} icon="shield">Permissions changed</Chip>);
        else if (p.includes("manage each other"))
          out.chips.push(
            <Chip key={p} icon="users">
              {p.includes("can't") ? "Can’t manage each other" : "Can manage each other"}
            </Chip>,
          );
        else rest.push(p);
      }
      break;
    }
    case "role_delete":
      out.text = <>deleted the role <Strong>{e.target}</Strong></>;
      break;
    case "channel_create":
      out.text = <>created {ch}</>;
      if (rest[0] === "encrypted") {
        rest.shift();
        out.chips.push(<Chip key="enc" icon="lock">End-to-end encrypted</Chip>);
      }
      break;
    case "channel_rename":
      out.text = <>renamed a channel to {ch}</>;
      break;
    case "channel_update": {
      const d = rest.shift() ?? "";
      const slow = /^slowmode (\d+)s$/.exec(d);
      // Entries from when encryption could be switched on an existing channel.
      if (d.startsWith("encryption ")) {
        out.icon = "lock";
        out.text = <>turned {d.endsWith("on") ? "on" : "off"} encryption in {ch}</>;
      } else if (d === "topic") {
        out.icon = "pencil";
        out.text = <>changed the topic of {ch}</>;
      } else if (slow) {
        out.icon = "clock";
        out.text =
          slow[1] === "0" ? <>turned off slowmode in {ch}</> : <>set slowmode in {ch} to <Strong>{duration(+slow[1])}</Strong></>;
      } else {
        out.text = <>changed retention or bitrate in {ch}</>;
      }
      break;
    }
    case "channel_delete":
      out.text = <>deleted <Strong>#{e.target}</Strong></>;
      for (const p of rest.splice(0))
        out.chips.push(<Chip key={p} icon="trash">{Number(p.split(" ")[0]).toLocaleString()} messages deleted</Chip>);
      break;
    case "channel_wipe":
      out.text = <>wiped {ch}</>;
      for (const p of rest.splice(0))
        out.chips.push(<Chip key={p} icon="trash">{Number(p.split(" ")[0]).toLocaleString()} messages deleted</Chip>);
      break;
    case "overwrite_set": {
      const [kind, id] = [e.target.slice(0, e.target.indexOf(":")), e.target.slice(e.target.indexOf(":") + 1)];
      const who =
        kind === "role" ? <RoleRef role={ctx.roleById(Number(id))} fallback={`a deleted role`} /> : member(id);
      out.text = <>changed permissions for {who}{chIn}</>;
      const d = rest.shift() ?? "";
      const m = /^allow (\d+) deny (\d+)$/.exec(d);
      if (d === "cleared") out.chips.push(<Chip key="clr" icon="refresh">Reset to the server permissions</Chip>);
      else if (m) {
        for (const l of permLabels(Number(m[1]))) out.chips.push(<Chip key={`a${l}`} icon="check" tone="success">{l}</Chip>);
        for (const l of permLabels(Number(m[2]))) out.chips.push(<Chip key={`d${l}`} icon="x" tone="error">{l}</Chip>);
      }
      break;
    }
    case "overwrite_sync": {
      const cat = (rest.shift() ?? "").replace(/^synced with category /, "");
      out.text = <>synced {ch} with <ChannelRef channel={ctx.channel(cat)} fallback={cat} /></>;
      break;
    }
    case "category_sync": {
      const on = rest.shift() === "on";
      out.text = (
        <>
          turned {on ? "on" : "off"} permission sync for <ChannelRef channel={ctx.channel(e.channelId)} fallback={e.target} />
        </>
      );
      break;
    }
    case "invite_create":
      out.text = <>created the invite <code className="font-mono text-[12px] text-text-primary">{e.target}</code></>;
      for (const p of rest.splice(0))
        out.chips.push(
          p.startsWith("for ") ? (
            <Chip key={p} icon="userPlus">For {p.slice(4)}</Chip>
          ) : (
            <Chip key={p} icon="users">{p === "unlimited" ? "Unlimited uses" : `Up to ${p.replace("max uses ", "")} uses`}</Chip>
          ),
        );
      break;
    case "invite_revoke":
      out.text = <>revoked the invite <code className="font-mono text-[12px] text-text-primary">{e.target}</code></>;
      break;
    case "server_update":
      out.text = <>updated the server profile</>;
      for (const p of rest.splice(0)) {
        if (p.startsWith("name: ")) out.chips.push(<Chip key={p} icon="pencil">Name: {p.slice(6)}</Chip>);
        else if (p.startsWith("public: ")) out.chips.push(<Chip key={p} icon="users">Public listing {p.slice(8)}</Chip>);
        else out.chips.push(<Chip key={p}>{p}</Chip>);
      }
      break;
    case "storage_min_free": {
      const bytes = Number(rest.shift() ?? 0);
      out.text = bytes ? <>set the upload headroom to <Strong>{formatFileSize(bytes)}</Strong></> : <>turned off the upload headroom</>;
      break;
    }
    case "ownership_transfer":
      out.text = <>transferred ownership to {member(e.target)}</>;
      break;
    default:
      out.text = (
        <>
          {e.action.replace(/_/g, " ")}
          {e.target ? <> <Strong>{e.target}</Strong></> : null}
          {chIn}
        </>
      );
  }
  for (const p of rest) out.chips.push(<Chip key={`r${p}`}>{p}</Chip>);
  return out;
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

function dayHeading(epoch: number): { label: string; sub?: string } {
  const d = new Date(epoch * 1000);
  const diff = Math.round((startOfDay(new Date()) - startOfDay(d)) / 86400000);
  const full = d.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" });
  if (diff === 0) return { label: "Today", sub: full };
  if (diff === 1) return { label: "Yesterday", sub: full };
  if (diff < 7) return { label: d.toLocaleDateString(undefined, { weekday: "long" }), sub: d.toLocaleDateString(undefined, { month: "long", day: "numeric" }) };
  return {
    label: d.toLocaleDateString(undefined, {
      month: "long",
      day: "numeric",
      year: d.getFullYear() === new Date().getFullYear() ? undefined : "numeric",
    }),
  };
}

/// Audit log (VIEW_AUDIT_LOG): newest first, paged by id. Entries come
/// from the server's audit_log table written by every moderation and
/// management handler, kept 180 days. Search and the action filter run
/// on the server (AuditLogRequest.query / .actions), so they cover the
/// whole log, not just the loaded pages.
export default function AuditLogTab({ serverId }: { serverId: string }) {
  const channels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  const roles = useChatStore((s) => s.rolesByServer[serverId] ?? EMPTY_LIST);
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [applied, setApplied] = useState("");
  const [category, setCategory] = useState<Category | "all">("all");
  // Answers to anything but the latest request are dropped; `replace`
  // says whether that request was page 1 of a new search.
  const seqRef = useRef(0);
  const answeredRef = useRef(0);
  const replaceRef = useRef(true);

  // Typing settles before it searches (the community rate-limits list
  // fetches); a category change applies at once.
  useEffect(() => {
    const q = query.trim();
    if (q === applied) return;
    const t = setTimeout(() => setApplied(q), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [query, applied]);

  const actions = useMemo(
    () => (category === "all" ? [] : Object.keys(ACTIONS).filter((a) => ACTIONS[a].cat === category)),
    [category],
  );

  const fetchPage = useCallback(
    (beforeId: number) => {
      const seq = ++seqRef.current;
      replaceRef.current = beforeId === 0;
      setLoading(true);
      invoke("list_audit_log", { serverId, beforeId, limit: 50, query: applied, actions, seq }).catch((err) => {
        if (seq !== seqRef.current) return;
        setLoading(false);
        setError(String(err));
      });
      setTimeout(() => {
        if (seq !== seqRef.current || answeredRef.current === seq) return;
        setLoading(false);
        setLoaded(true);
        setError("The server didn't answer. It may be running an older version without audit search.");
      }, NO_ANSWER_MS);
    },
    [serverId, applied, actions],
  );

  // A new search keeps the old entries on screen until its answer lands.
  useEffect(() => {
    fetchPage(0);
  }, [fetchPage]);

  useEffect(() => {
    const un = listen<AuditLogReceivedPayload>("audit_log_received", (event) => {
      const p = event.payload;
      if (p.serverId !== serverId || p.seq !== seqRef.current) return;
      answeredRef.current = p.seq;
      setLoading(false);
      setLoaded(true);
      if (!p.success) {
        setError(p.message || "Couldn't load the audit log.");
        return;
      }
      setError(null);
      const replace = replaceRef.current;
      setEntries((prev) => (replace ? p.entries : [...prev, ...p.entries]));
      setHasMore(p.hasMore);
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId]);

  const ctx = useMemo<Ctx>(
    () => ({
      serverId,
      channel: (id) => channels.find((c) => c.id === id),
      roleById: (id) => (id === -1 ? roles.find((r) => r.isDefault) : roles.find((r) => r.id === id)),
      roleByName: (name) => roles.find((r) => !r.isDefault && r.name === name),
    }),
    [serverId, channels, roles],
  );

  const days = useMemo(() => {
    const out: Array<{ key: number; entries: AuditEntry[] }> = [];
    for (const e of entries) {
      const key = startOfDay(new Date(e.timestamp * 1000));
      if (out.length && out[out.length - 1].key === key) out[out.length - 1].entries.push(e);
      else out.push({ key, entries: [e] });
    }
    return out;
  }, [entries]);

  const filtering = !!applied || category !== "all";

  return (
    <div className="flex flex-col">
      <div className="sticky top-0 z-20 -mx-2 flex items-center gap-2 bg-bg-dark px-2 pb-4">
        <SearchField
          value={query}
          onChange={setQuery}
          placeholder="Search by member, role, channel or reason"
          className="min-w-0 flex-1"
        />
        <div className="w-48 shrink-0">
        <Select
          aria-label="Filter by action"
          value={category}
          onChange={setCategory}
          options={CATEGORY_OPTIONS.map((o) => ({
            value: o.value,
            label: o.label,
            icon: <Icon name={o.icon} size={14} className="text-text-muted" />,
          }))}
        />
        </div>
      </div>

      {error && (
        <div className="mb-4 flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
          <Icon name="alert" size={15} />
          {error}
        </div>
      )}

      {!loaded ? (
        <p className="py-6 text-[13px] text-text-muted">Loading the audit log…</p>
      ) : entries.length === 0 && !filtering && !error ? (
        <EmptyState
          icon="file"
          title="Nothing logged yet"
          body="Moderation and settings changes show up here, newest first. Entries are kept for 180 days."
        />
      ) : entries.length === 0 && filtering ? (
        <EmptyState
          icon="search"
          title="No matching entries"
          body="Nothing in the last 180 days matches. Try another search or action type."
          action={
            <button
              type="button"
              onClick={() => {
                setQuery("");
                setApplied("");
                setCategory("all");
              }}
              className="rounded-sm bg-bg-lighter px-4 py-2 text-[13px] font-semibold text-text-primary hover:bg-surface-active"
            >
              Clear filters
            </button>
          }
        />
      ) : (
        <div className="flex flex-col gap-6">
          {days.map((d) => {
            const h = dayHeading(d.key / 1000);
            return (
              <section key={d.key}>
                <div className="mb-1 flex items-baseline gap-2">
                  <SectionLabel>{h.label}</SectionLabel>
                  {h.sub && <span className="font-meta text-meta text-text-muted">{h.sub}</span>}
                </div>
                <div className="relative">
                  {/* The day's rail, threaded behind the action tiles. */}
                  <div className="absolute bottom-6 left-[15px] top-6 w-0.5 rounded-full bg-border-divider" />
                  {d.entries.map((e) => (
                    <AuditRow key={e.id} entry={e} ctx={ctx} />
                  ))}
                </div>
              </section>
            );
          })}
          <div className="flex flex-col items-center gap-2 pb-2 pt-1">
            {hasMore ? (
              <button
                type="button"
                onClick={() => fetchPage(entries[entries.length - 1]?.id ?? 0)}
                disabled={loading}
                className="rounded-sm border border-border px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-60"
              >
                {loading ? "Loading…" : "Load older entries"}
              </button>
            ) : (
              loaded &&
              !filtering && (
                <span className="text-[12px] text-text-muted">
                  That’s everything. Entries are kept for 180 days.
                </span>
              )
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function AuditRow({ entry: e, ctx }: { entry: AuditEntry; ctx: Ctx }) {
  const d = describe(e, ctx);
  const when = new Date(e.timestamp * 1000);
  return (
    <div className="relative flex gap-3.5 py-2.5">
      <span className="relative z-10 flex h-8 w-8 shrink-0 rounded-md bg-bg-dark">
        <span className={`flex h-full w-full items-center justify-center rounded-md ${TONE_TILE[d.tone]}`}>
          <Icon name={d.icon} size={15} />
        </span>
      </span>
      <div className="min-w-0 flex-1 pt-1.5">
        <div className="text-[13px] leading-5 text-text-secondary">
          <span className="mr-1.5 inline-flex translate-y-[3px]">
            <UserAvatar uid={e.actorUid ?? 0} name={e.actor} size={18} />
          </span>
          <MemberRef serverId={ctx.serverId} username={e.actor} /> {d.text}
        </div>
        {d.reason && (
          <div className="mt-1.5 border-l-2 border-bg-surface pl-2.5 text-[13px] leading-5 text-text-secondary">
            {d.reason}
          </div>
        )}
        {d.chips.length > 0 && <div className="mt-2 flex flex-wrap gap-1.5">{d.chips}</div>}
      </div>
      <time
        dateTime={when.toISOString()}
        title={when.toLocaleString(undefined, { dateStyle: "full", timeStyle: "medium" })}
        className="shrink-0 pt-1.5 font-meta text-meta leading-5 tabular-nums text-text-muted"
      >
        {when.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}
      </time>
    </div>
  );
}

export function EmptyState({
  icon,
  title,
  body,
  action,
}: {
  icon: IconName;
  title: string;
  body: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center px-6 py-14 text-center">
      <span className="mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-bg-light text-text-muted shadow-raised">
        <Icon name={icon} size={20} />
      </span>
      <div className="mb-1 font-display text-[16px] font-semibold text-text-primary">{title}</div>
      <p className="mb-4 max-w-sm text-[13px] leading-[1.55] text-text-muted">{body}</p>
      {action}
    </div>
  );
}

