import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import ServerBrowseView from "../features/servers/ServerBrowseView";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import ChatPanel from "../features/chat/ChatPanel";
import FriendsList from "../features/friends/FriendsList";
import MembersList from "../features/friends/MembersList";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useServerEvents } from "../features/servers/useServerEvents";
import { useFriendsEvents } from "../features/friends/useFriendsEvents";
import { useUiStore } from "../stores/uiStore";
import VoicePanel from "../features/voice/VoicePanel";
import { useVoiceEvents } from "../features/voice/useVoiceEvents";
import DmChatPanel from "../features/dm/DmChatPanel";
import UserProfilePopup from "../features/dm/UserProfilePopup";
import UserContextMenu from "../features/voice/UserContextMenu";
import SettingsModal from "../features/settings/SettingsModal";
import { useDmEvents } from "../features/dm/useDmEvents";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();
  useServerEvents();
  useFriendsEvents();
  useVoiceEvents();
  useDmEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);
  const activeView = useUiStore((s) => s.activeView);
  const membersPanelVisible = useUiStore((s) => s.membersPanelVisible);
  const dmFriendsPanelVisible = useUiStore((s) => s.dmFriendsPanelVisible);

  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {/* Connection warning */}
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 shrink-0 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      {/* Horizontal server tab bar */}
      <ServerBar />

      {/* Main content row */}
      <div className="flex flex-1 overflow-hidden">
        <DmSidebar />

        {activeView === "browse" ? (
          <ServerBrowseView />
        ) : (
          <>
            <ChannelSidebar />
            {activeView === "voice" ? (
              <VoicePanel />
            ) : activeView === "dm" ? (
              <>
                <DmChatPanel />
                {dmFriendsPanelVisible && <FriendsList />}
              </>
            ) : activeView === "home" ? (
              <>
                <ChatPanel />
                <FriendsList />
              </>
            ) : (
              <>
                <ChatPanel />
                {membersPanelVisible && <MembersList />}
              </>
            )}
          </>
        )}
      </div>

      <ServerDiscoveryModal />
      <UserProfilePopup />
      <UserContextMenu />
      <SettingsModal />
    </div>
  );
}
