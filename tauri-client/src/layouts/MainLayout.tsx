import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useServerEvents } from "../features/servers/useServerEvents";
import { useFriendsEvents } from "../features/friends/useFriendsEvents";
import { useUiStore } from "../stores/uiStore";
import ChannelSidebar from "../features/channels/ChannelSidebar";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();
  useServerEvents();
  useFriendsEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);

  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}
      <div className="flex flex-1 overflow-hidden">
        <DmSidebar />
        <div className="flex flex-1 flex-col overflow-hidden">
          <ServerBar />
          <div className="flex flex-1 overflow-hidden">
            <ChannelSidebar />
            {/* Chat Panel placeholder - Task 8 */}
            <div className="flex flex-1 items-center justify-center bg-bg-secondary">
              <span className="text-sm text-text-muted">Select a channel to start chatting</span>
            </div>
            {/* Right Panel placeholder - Task 9 */}
            <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary p-4">
              <span className="text-sm text-text-muted">Friends / Members...</span>
            </div>
          </div>
        </div>
      </div>
      <ServerDiscoveryModal />
    </div>
  );
}
