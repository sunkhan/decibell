// Shared by the shell pages (main-server, main-home): the renderer's real MainLayout fed the GPUI
// shell's fixture (crates/decibell-shell/src/fixture.rs, exported as JSON by
// `cargo run -p decibell-shell --features fixtures --bin decibell-shell-fixture -- pages/fixtures`)
// through the renderer's own event hooks: the events are delivered as the preload bridge's
// `listen` would deliver them, so the stores fill exactly as in the app.
import { useEffect, type ReactNode } from "react";
import { MemoryRouter } from "react-router-dom";
import MainLayout from "@app/layouts/MainLayout";
import { useUserDirectory } from "@app/features/users/userDirectory";
import { useAuthEvents } from "@app/features/auth/useAuthEvents";
import { useServerEvents } from "@app/features/servers/useServerEvents";
import { useChatEvents } from "@app/features/chat/useChatEvents";
import { useSearchEvents } from "@app/features/search/useSearchEvents";
import { useFriendsEvents } from "@app/features/friends/useFriendsEvents";
import { usePresenceEvents } from "@app/features/presence/usePresenceEvents";
import { useVoiceEvents } from "@app/features/voice/useVoiceEvents";
import { useChatStore } from "@app/stores/chatStore";
import { useUiStore } from "@app/stores/uiStore";
import { useVoiceStore } from "@app/stores/voiceStore";
import { frames } from "../runtime/harness";

export type ShellFixture = {
  scene: string;
  me: { uid: number; username: string };
  events: { event: string; payload: unknown }[];
  nav: {
    view: "home" | "server";
    server: string | null;
    channel: string | null;
    voice: { server: string; channel: string } | null;
  };
};

type Handler = (e: { event: string; payload: unknown; id: number }) => void;
const listeners = new Map<string, Set<Handler>>();

// The bridge's `listen`, keeping the handlers so the fixture can be delivered to them. Everything
// else stays the harness stub.
const stub = (window as unknown as { decibell: Record<string, unknown> }).decibell;
(window as unknown as { decibell: unknown }).decibell = new Proxy(stub, {
  get(target, prop) {
    if (prop === "listen") {
      return (name: string, cb: Handler) => {
        let set = listeners.get(name);
        if (!set) listeners.set(name, (set = new Set()));
        set.add(cb);
        return Promise.resolve(() => set!.delete(cb));
      };
    }
    return (target as Record<string | symbol, unknown>)[prop];
  },
});

let id = 0;
/// Deliver one event to the hooks (the settings pages answer their requests through it).
export function deliver(event: string, payload: unknown) {
  for (const cb of listeners.get(event) ?? []) cb({ event, payload, id: ++id });
}

/// The hooks App.tsx mounts for every route.
function Hooks() {
  useUserDirectory();
  useAuthEvents();
  useServerEvents();
  useChatEvents();
  useSearchEvents();
  useFriendsEvents();
  usePresenceEvents();
  useVoiceEvents();
  return null;
}

function Mounted({ onMount, children }: { onMount: () => void; children: ReactNode }) {
  useEffect(onMount, []);
  return <>{children}</>;
}

export function ShellPage() {
  return (
    <MemoryRouter>
      <Mounted onMount={() => {}}>
        <Hooks />
        <MainLayout />
      </Mounted>
    </MemoryRouter>
  );
}

/// Replay the fixture, then show its navigation (what the GPUI fixture's `apply` does).
export async function replay(fixture: ShellFixture) {
  await frames(2);
  // The crash-report notice was dismissed long ago (as in the GPUI fixture).
  useUiStore.getState().setCrashReportingConsentShown(true);
  // In voice (our own join: not an event): the roster events below then fill the participants.
  if (fixture.nav.voice) {
    useVoiceStore.getState().enterChannel(fixture.nav.voice.server, fixture.nav.voice.channel, {});
  }
  for (const { event, payload } of fixture.events) deliver(event, payload);
  await frames(2);
  const chat = useChatStore.getState();
  if (fixture.nav.view === "server" && fixture.nav.server) {
    chat.setActiveServer(fixture.nav.server);
    useUiStore.getState().setActiveView("server");
    chat.setActiveChannel(null);
    if (fixture.nav.channel) chat.setActiveChannel(fixture.nav.channel);
  } else {
    chat.setActiveServer(null);
    chat.setActiveChannel(null);
    useUiStore.getState().setActiveView("home");
  }
  await frames(4);
  return { scene: fixture.scene, events: fixture.events.length };
}
