import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { invoke, listen } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import type { ServerInfoPayload } from "../../types";

export function useAuthEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenSuccess = listen<{ username: string }>(
      "login_succeeded",
      (event) => {
        useAuthStore.getState().login(event.payload.username);
        navigate("/");
        // Pull the server list immediately after login so ServerBar +
        // ServerBrowseView have data to render. Response arrives via
        // `server_list_received` below.
        invoke("request_server_list").catch(() => {});
      },
    );

    const unlistenFailed = listen<{ message: string }>(
      "login_failed",
      (event) => {
        useAuthStore.getState().setLoginError(event.payload.message);
      },
    );

    const unlistenRegister = listen<{ success: boolean; message: string }>(
      "register_responded",
      (event) => {
        useAuthStore.getState().setRegisterResult(event.payload);
      },
    );

    const unlistenServerList = listen<{ servers: ServerInfoPayload[] }>(
      "server_list_received",
      (event) => {
        // Coerce numeric wire ids to strings — the renderer keys all
        // server-id-shaped state by string (Set members, Record keys,
        // React list keys). Doing the conversion at the listener
        // boundary keeps every downstream consumer simple.
        const servers = event.payload.servers.map((s) => ({
          id: String(s.id),
          name: s.name,
          description: s.description,
          hostIp: s.hostIp,
          port: s.port,
          memberCount: s.memberCount,
        }));
        useChatStore.getState().setServers(servers);
      },
    );

    const unlistenLoggedOut = listen("logged_out", () => {
      useAuthStore.getState().logout();
      useChatStore.getState().resetForLogout();
      useUiStore.getState().setActiveView("home");
      navigate("/login");
    });

    return () => {
      unlistenSuccess.then((fn) => fn());
      unlistenFailed.then((fn) => fn());
      unlistenRegister.then((fn) => fn());
      unlistenServerList.then((fn) => fn());
      unlistenLoggedOut.then((fn) => fn());
    };
  }, [navigate]);
}
