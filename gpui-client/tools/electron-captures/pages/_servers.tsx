// Shared by the server pages (browse*, join-dialog-*, invite-*, server-menu-*, leave-confirm,
// create-channel-*, channel-menu, sidebar-dragging-*, category-sync-prompt, welcome-*): the
// renderer's real MainLayout signed in as perf1 from the shell's fixture, then the GPUI servers
// fixture (crates/decibell-servers/src/fixture.rs, exported by `cargo run -p decibell-servers
// --features fixtures --bin decibell-servers-fixture -- pages/fixtures`): its extra events through
// the event hooks, then what the user did — the view, a typed search, an open dialog or menu
// (clicked, as the user would), a drag (the HTML5 drag events the sidebar listens to). Drawn as
// the whole window (`Framed`, 1280 × 800).
import { Framed } from "./_frame";
import { ShellPage, replay, deliver, type ShellFixture } from "./_shell";
import { frames, respond, sleep } from "../runtime/harness";
import { useUiStore } from "@app/stores/uiStore";
import { useChatStore } from "@app/stores/chatStore";
import { useInviteResolveStore } from "@app/stores/inviteResolveStore";

type Ui = {
  view?: "browse";
  search?: string;
  pendingInvite?: { code: string };
  inviteResolve?: Record<string, { status: "loading" | "done"; resolved: unknown; error: string | null; invalid: boolean }>;
  modal?: string;
  inviteCreating?: boolean;
  friendsInCommunity?: { known: boolean; memberUids: number[] };
  friendRows?: { sent: number[]; member: number[] };
  menu?: "server";
  click?: string;
  contextMenu?: { at: [number, number]; on: string | null };
  categoryPlus?: string;
  createName?: string;
  encrypted?: boolean;
  drag?: { from: string; over: string; top: boolean; at: [number, number]; drop?: boolean };
  channel?: string;
};

export type ServersFixture = {
  shell: ShellFixture;
  extra: { event: string; payload: unknown }[];
  ui: Ui;
};

export function ServersPage() {
  return (
    <Framed>
      <ShellPage />
    </Framed>
  );
}

/// The page's CSS px for a point the fixture gives in the framed window (they are the same: the
/// framed page is the 1280 × 800 window).
const button = (text: string) =>
  [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;

function setValue(input: HTMLInputElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  set.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function row(id: string): HTMLElement {
  const el = document.querySelector(`[data-reorder-id="${id}"]`) as HTMLElement | null;
  if (!el) throw new Error(`no row ${id}`);
  return el;
}

export async function openStage(fixture: ServersFixture) {
  const ui = fixture.ui;
  // What native answers.
  if (ui.friendsInCommunity) respond("friendsInCommunity", ui.friendsInCommunity);
  // A lookup the fixture leaves out stays out.
  respond("resolveInviteCode", () => new Promise(() => {}));
  if (ui.inviteCreating) respond("createInvite", () => new Promise(() => {}));
  if (ui.friendRows) {
    const rows = ui.friendRows;
    respond("createInvite", (args: unknown) => {
      const a = args as { serverId: string; forUid: number; forUser: string; requestId: number };
      const member = rows.member.includes(a.forUid);
      setTimeout(() =>
        deliver("invite_create_responded", {
          serverId: a.serverId,
          success: !member,
          message: member ? `${a.forUser} is already a member.` : "",
          invite: member ? null : { code: "P3RS0NAL01", createdBy: "perf1", createdByUid: 6, createdAt: 0, expiresAt: 0, maxUses: 1, uses: 0, forUser: a.forUser, forUid: a.forUid },
          requestId: a.requestId,
          alreadyMember: member,
        }),
      );
      return null;
    });
  }

  await replay(fixture.shell);
  for (const { event, payload } of fixture.extra) deliver(event, payload);
  await frames(2);

  const chat = useChatStore.getState();
  const store = useUiStore.getState();
  if (ui.view === "browse") store.setActiveView("browse");
  if (ui.channel) {
    chat.setActiveChannel(ui.channel);
    store.setActiveView("server");
  }
  await frames(4);
  if (ui.search) {
    setValue(document.querySelector("input[placeholder='Search servers...']") as HTMLInputElement, ui.search);
  }
  if (ui.inviteResolve) useInviteResolveStore.setState({ entries: { ...ui.inviteResolve, } as never });
  if (ui.pendingInvite) chat.setPendingInvite(ui.pendingInvite);
  if (ui.modal) store.openModal(ui.modal);
  await frames(4);
  if (ui.inviteCreating) button("Create Invite")?.click();
  if (ui.friendRows) {
    await sleep(50);
    for (const uid of [...ui.friendRows.sent, ...ui.friendRows.member]) {
      // The rows are sorted by name: find the uid's row by its avatar's key isn't exposed; the
      // fixture's names are unique, so go through the friend list.
      const name = (fixture.shell.events
        .concat(fixture.extra)
        .filter((e) => e.event === "friend_list_received")
        .flatMap((e) => (e.payload as { friends: { uid: number; username: string }[] }).friends)
        .find((f) => f.uid === uid) ?? { username: "" }).username;
      const nameEl = [...document.querySelectorAll("span")].find((s) => s.textContent === name);
      const rowEl = nameEl?.parentElement;
      (rowEl?.querySelector("button") as HTMLButtonElement | null)?.click();
      await sleep(30);
    }
  }
  if (ui.menu === "server") {
    (document.querySelector("button[title='Server options']") as HTMLButtonElement | null)?.click();
    await frames(2);
  }
  if (ui.contextMenu) {
    const [x, y] = ui.contextMenu.at;
    const target = ui.contextMenu.on ? row(ui.contextMenu.on) : (document.querySelector(".fade-end") as HTMLElement);
    target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: x, clientY: y, button: 2 }));
    await frames(2);
  }
  if (ui.categoryPlus) {
    (row(ui.categoryPlus).querySelector("[title='Create channel in category']") as HTMLElement | null)?.click();
    await frames(2);
  }
  if (ui.click) {
    button(ui.click)?.click();
    await frames(2);
  }
  if (ui.createName !== undefined || ui.encrypted) {
    const input = document.querySelector("input[maxlength='64']") as HTMLInputElement | null;
    if (input && ui.createName) setValue(input, ui.createName);
    if (ui.encrypted) (document.querySelector("[role='switch']") as HTMLElement | null)?.click();
    await frames(2);
  }
  if (ui.drag) {
    const d = ui.drag;
    const dt = new DataTransfer();
    const from = row(d.from);
    from.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
    await frames(2);
    const over = row(d.over);
    const r = over.getBoundingClientRect();
    const y = d.top ? r.top + r.height * 0.25 : r.top + r.height * 0.75;
    over.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt, clientX: d.at[0], clientY: y }));
    await frames(2);
    if (d.drop) {
      over.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt, clientX: d.at[0], clientY: y }));
      await frames(2);
    }
  }
  await frames(4);
  // The GPUI captures show nothing focused.
  await sleep(120);
  (document.activeElement as HTMLElement | null)?.blur();
  await frames(2);
  return { framed: true };
}
