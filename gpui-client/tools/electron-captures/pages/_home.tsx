// Shared by the people pages (home-friends-*, dm-*, profile-*, custom-status, user-menu-*): the
// renderer's real MainLayout fed the GPUI people fixture (crates/decibell-home/src/fixture.rs,
// exported by `cargo run -p decibell-home --features fixtures --bin decibell-home-fixture --
// pages/fixtures`): the shell's events plus the people scene's, delivered through the bridge's
// `listen` (`_shell.tsx`), then what the scene sets in the renderer's stores — the E2EE markers,
// the `e2ee_peer_info` answers, the navigation, the Friends page's tab (a click: it is the page's
// own state), the overlay.
import { ShellPage, replay, type ShellFixture } from "./_shell";
import { Framed, TITLEBAR } from "./_frame";
import { frames, respond, sleep } from "../runtime/harness";
import { useUiStore } from "@app/stores/uiStore";
import { useDmStore } from "@app/stores/dmStore";
import { useE2eeStore } from "@app/stores/e2eeStore";

type Overlay =
  | { kind: "profile-popup"; uid: number; server: string; anchor: { x: number; y: number } }
  | { kind: "profile"; uid: number; server: string | null; tab: "profile" | "privacy" }
  | { kind: "custom-status" }
  | { kind: "user-menu"; uid: number; server: string; anchor: { x: number; y: number } }
  | { kind: "nickname" | "ban"; uid: number; server: string }
  | null;

export type HomeFixture = Omit<ShellFixture, "nav"> & {
  nav: { view: "home" | "server" | "dm"; server: string | null; channel: string | null; dm: number | null };
  friends: { filter: "all" | "online" | "pending" | "blocked"; addOpen: boolean };
  rail: boolean;
  e2ee: { changedPeers: Record<string, number> };
  peerInfo: Record<string, unknown>;
  overlay: Overlay;
};

export const HomePage = ShellPage;

/// The pages whose overlay places itself against the whole window (the popout, the menu, the
/// profile screen, the custom status dialog): drawn under a title-bar band, as the GPUI window is.
export function FramedHomePage() {
  return (
    <Framed>
      <ShellPage />
    </Framed>
  );
}

function buttonByText(text: string): HTMLElement {
  const found = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim().startsWith(text));
  if (!found) throw new Error(`no button "${text}"`);
  return found as HTMLElement;
}

/// `framed`: the page is drawn under the title-bar band (anchors move down by it; pairs.mjs crops
/// the band off).
export async function replayHome(f: HomeFixture, framed = false) {
  respond("e2eePeerInfo", (args: unknown) => {
    const uid = (args as { uid?: number } | undefined)?.uid ?? 0;
    return f.peerInfo[String(uid)] ?? { hasKeys: false, keyId: 0, fingerprint: "", safetyNumber: "", changedAt: 0, pinned: false };
  });
  useUiStore.setState({ dmFriendsPanelVisible: f.rail });
  const view = f.nav.view === "server" ? "server" : "home";
  const out = await replay({ ...f, nav: { view, server: f.nav.server, channel: f.nav.channel, voice: null } });
  // The passphrase prompt the first "locked" opens is the settings agent's in the GPUI client.
  useE2eeStore.getState().closePassphraseModal();
  const changed: Record<number, number> = {};
  for (const [uid, at] of Object.entries(f.e2ee.changedPeers)) changed[Number(uid)] = at;
  useE2eeStore.setState({ changedPeers: changed });
  if (f.nav.dm) {
    useDmStore.getState().setActiveDmUser(f.nav.dm);
    useUiStore.getState().setActiveView("dm");
  }
  await frames(4);
  if (f.nav.view === "home") {
    const label = { all: "All", online: "Online", pending: "Pending", blocked: "Blocked" }[f.friends.filter];
    if (f.friends.filter !== "all") buttonByText(label).click();
    if (f.friends.addOpen) buttonByText("Add Friend").click();
  }
  const o = f.overlay;
  const shift = (a: { x: number; y: number }) => (framed ? { x: a.x, y: a.y + TITLEBAR } : a);
  const ui = useUiStore.getState();
  if (o?.kind === "profile-popup") ui.openProfilePopup(o.uid, shift(o.anchor), o.server);
  if (o?.kind === "profile") {
    ui.openUserProfile(o.uid, o.server);
    await frames(2);
    if (o.tab === "privacy") buttonByText("Privacy").click();
  }
  if (o?.kind === "custom-status") ui.openModal("custom-status");
  if (o?.kind === "user-menu") ui.openContextMenu(o.uid, shift(o.anchor), o.server);
  if (o?.kind === "nickname" || o?.kind === "ban") {
    // No twin in the renderer's user menu: these live in Server Settings → Members.
  }
  await sleep(50);
  await frames(4);
  return { ...out, scene: f.scene, framed };
}
