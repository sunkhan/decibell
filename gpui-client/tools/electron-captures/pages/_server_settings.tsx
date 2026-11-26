// Shared by the settings pages (server-settings-*, channel-settings-*, color-picker,
// transfer-ownership, picture-editor): the renderer's real MainLayout on the GPUI settings fixture
// (crates/decibell-server-settings/src/fixture.rs: the shell's fixture plus what the windows show,
// exported by `cargo run -p decibell-server-settings --features fixtures --bin
// decibell-server-settings-fixture -- pages/fixtures`), drawn as the whole window (the modal covers
// the title bar, as in the app), with the stub's `invoke` answered the way the GPUI fixture's
// community server answers: search, the audit log, storage, bans, roles, overwrites. "Now" is the
// fixture's (the role preview's clock, the time-out countdown, the audit log's days).
import fixture from "./fixtures/settings.json";
import { ShellPage, deliver, replay, type ShellFixture } from "./_shell";
import { Framed } from "./_frame";
import { respond, frames, sleep, $ } from "../runtime/harness";
import { useUiStore } from "@app/stores/uiStore";

type Member = { username: string; nickname: string; roleIds: number[]; uid: number };
type Entry = { id: number; actor: string; target: string; details: string; channelId: string; action: string };
type SettingsFixture = ShellFixture & { now: number; members: Member[]; audit: Entry[]; storage: unknown };
const F = fixture as unknown as SettingsFixture;
const SID = "1";

// The fixture's clock: `Date.now()` and `new Date()` read it.
const NOW_MS = F.now * 1000;
const RealDate = Date;
class FixedDate extends RealDate {
  constructor(...args: unknown[]) {
    if (args.length === 0) super(NOW_MS);
    else super(...(args as [number]));
  }
  static now() {
    return NOW_MS;
  }
}
(globalThis as unknown as { Date: DateConstructor }).Date = FixedDate as unknown as DateConstructor;

const later = (event: string, payload: unknown) => setTimeout(() => deliver(event, payload), 0);

respond("searchMembers", (args: { query: string; roleId: number; after: string; limit: number; seq: number }) => {
  const q = (args.query ?? "").toLowerCase();
  const found = F.members
    .filter(
      (m) =>
        (args.roleId === -1 ? m.roleIds.length === 0 : args.roleId > 0 ? m.roleIds.includes(args.roleId) : true) &&
        (!q || m.username.toLowerCase().includes(q) || m.nickname.toLowerCase().includes(q)),
    )
    .sort((a, b) => (a.username < b.username ? -1 : a.username > b.username ? 1 : 0));
  const start = args.after ? found.findIndex((m) => m.username > args.after) : 0;
  const from = start < 0 ? found.length : start;
  const page = found.slice(from, from + args.limit);
  later("member_search_received", {
    serverId: SID,
    success: true,
    message: "",
    members: page,
    totalMatches: found.length,
    hasMore: from + page.length < found.length,
    nextAfter: page.length ? page[page.length - 1].username : "",
    seq: args.seq,
    firstPage: !args.after,
  });
  return null;
});

respond("listAuditLog", (args: { beforeId: number; limit: number; query: string; actions: string[]; seq: number }) => {
  const q = (args.query ?? "").toLowerCase();
  const all = F.audit.filter(
    (e) =>
      (!args.beforeId || e.id < args.beforeId) &&
      (!args.actions?.length || args.actions.includes(e.action)) &&
      (!q || [e.actor, e.target, e.details, e.channelId].some((f) => f.toLowerCase().includes(q))),
  );
  later("audit_log_received", { serverId: SID, success: true, message: "", entries: all.slice(0, args.limit), hasMore: all.length > args.limit, seq: args.seq });
  return null;
});

const fixtureEvent = (name: string, match?: (p: Record<string, unknown>) => boolean) =>
  F.events.filter((e) => e.event === name && (!match || match(e.payload as Record<string, unknown>))).map((e) => e.payload);

respond("getStorageInfo", () => (later("storage_info_received", F.storage), null));
respond("listBans", () => (fixtureEvent("ban_list_received").forEach((p) => later("ban_list_received", p)), null));
respond("listRoles", () => {
  const lists = fixtureEvent("role_list_received");
  later("role_list_received", lists[lists.length - 1]);
  return null;
});
respond("listChannelOverwrites", (args: { channelId: string }) => {
  fixtureEvent("channel_overwrites_received", (p) => p.channelId === args.channelId).forEach((p) => later("channel_overwrites_received", p));
  return null;
});

export function SettingsPage() {
  return (
    <Framed>
      <ShellPage />
    </Framed>
  );
}

export const size = { width: 1280, height: 800 };

/// The button whose text is `text` (a nav item, a role row, a section tab).
export function byText(text: string, selector = "button"): HTMLElement {
  const el = Array.from(document.querySelectorAll<HTMLElement>(selector)).find((b) => b.textContent?.trim() === text || b.getAttribute("aria-label") === text);
  if (!el) throw new Error(`no ${selector} reads ${text}`);
  return el;
}

/// Type into a React-controlled input as the user would.
export function typeInto(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/// The fixture replayed, then Server Settings open on `tab` (its nav item's label).
export async function openServer(tab = "Overview") {
  await replay(F);
  useUiStore.getState().openModal("server-settings");
  await frames(4);
  if (tab !== "Overview") byText(tab).click();
  await sleep(60);
  await frames(4);
  return { framed: true };
}

/// The fixture replayed, then Channel Settings of `channel` open on `tab`.
export async function openChannel(channel: string, tab = "Overview") {
  await replay(F);
  useUiStore.getState().openChannelSettings(channel);
  await frames(4);
  if (tab !== "Overview") byText(tab).click();
  await sleep(60);
  await frames(4);
  return { framed: true };
}

/// A role in the Roles tab's hierarchy, then one of the editor's sections.
export async function selectRole(name: string, section: "Display" | "Permissions" | "Members") {
  const row = Array.from(document.querySelectorAll<HTMLElement>("aside button")).find((b) => b.querySelector("span.truncate")?.textContent === name);
  if (!row) throw new Error(`no role row ${name}`);
  row.click();
  await frames(2);
  const tab = Array.from(document.querySelectorAll<HTMLElement>("[role=tab]")).find((t) => t.textContent?.startsWith(section));
  tab?.click();
  await sleep(60);
  await frames(4);
}

export { $, frames, sleep };
