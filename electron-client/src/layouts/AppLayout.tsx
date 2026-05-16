import { useEffect } from "react";
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

    let unlistenFn: (() => void) | null = null;
    listen<UpdateEventPayload>("update_status", (event) => {
      const p = event.payload;
      useUpdateStore.getState().setFromEvent(
        p.status,
        p.mode,
        p.currentVersion,
      );
    }).then((u) => {
      unlistenFn = u;
    });
    return () => {
      if (unlistenFn) unlistenFn();
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
    return useChatStore.subscribe((state) => {
      apply(state.connectedServers.size);
    });
  }, []);

  return (
    <div className="relative flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
      <ToastStack />
    </div>
  );
}
