import { useEffect } from "react";
import CertMismatchModal from "../components/CertMismatchModal";
import { Outlet } from "react-router-dom";
import * as Sentry from "@sentry/electron/renderer";
import Titlebar from "./Titlebar";
import ToastStack from "../components/ToastStack";
import { listen } from "../lib/ipc";
import { useChatStore } from "../stores/chatStore";
import {
  useUpdateStore,
  type UpdateStatus,
  type UpdateMode,
} from "../stores/updateStore";

// Always-on chrome wrapper. Both /login and / sit inside this layout
// so the custom Titlebar (with min/max/close) stays present from the
// moment the window opens. The outlet renders the active route's
// content beneath the titlebar. ToastStack is also mounted here so
// notifications appear on /login as well as /.

interface UpdateEventPayload {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
}

export default function AppLayout() {
  useEffect(() => {
    // Pull the current snapshot first — covers the case where
    // initUpdater()'s boot-time broadcast fired before this listener
    // attached. After this, every subsequent transition arrives via
    // the 'update_status' event below.
    window.decibell.update.getStatus().then((snap) => {
      useUpdateStore.getState().setFromEvent(
        snap.status,
        snap.mode,
        snap.currentVersion,
      );
    });

    // Hold the promise, not the resolved function: unmounting before
    // listen() settles used to leave `unlistenFn` null, so the cleanup
    // no-opped and the listener leaked — a remount then handled every
    // update_status twice. Same form useChatEvents/useDmEvents use.
    const unlisten = listen<UpdateEventPayload>("update_status", (event) => {
      const p = event.payload;
      useUpdateStore.getState().setFromEvent(
        p.status,
        p.mode,
        p.currentVersion,
      );
    });
    return () => {
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Track how many community servers this install is connected to.
  // Helps reproduce "happens when N+ servers connected" bug reports.
  // Sentry.setTag is a scope mutation, not a network call, so it's
  // safe to invoke whether or not initRendererSentry actually fired.
  useEffect(() => {
    const apply = (size: number) => {
      Sentry.setTag("connected_servers", String(size));
    };
    apply(useChatStore.getState().connectedServers.size);
    // Only on an actual change: setTag notifies Sentry's scope listeners,
    // and the Electron SDK serialises the whole scope to main over IPC each
    // time — this subscription fires on every chatStore write (each
    // message, each resize tick).
    return useChatStore.subscribe((state, prev) => {
      if (state.connectedServers !== prev.connectedServers) {
        apply(state.connectedServers.size);
      }
    });
  }, []);

  return (
    // No transition on the root: easing the palette swap here faded
    // inherited text across the whole tree (a per-frame style recalc of
    // every node) while token-coloured surfaces snapped anyway — a
    // low-contrast flash, not a fade. The swap snaps, like the rest.
    <div className="relative flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
      <ToastStack />
      <CertMismatchModal />
    </div>
  );
}
