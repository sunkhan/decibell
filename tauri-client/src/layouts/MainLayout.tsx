import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import ChatPanel from "../features/chat/ChatPanel";
import FriendsList from "../features/friends/FriendsList";
import MembersList from "../features/friends/MembersList";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useServerEvents } from "../features/servers/useServerEvents";
import { useFriendsEvents } from "../features/friends/useFriendsEvents";
import { useUiStore } from "../stores/uiStore";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();
  useServerEvents();
  useFriendsEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);
  const activeView = useUiStore((s) => s.activeView);

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
            <ChatPanel />
            {activeView === "home" ? <FriendsList /> : <MembersList />}
          </div>
        </div>
      </div>

      <ServerDiscoveryModal />
    </div>
  );
}
