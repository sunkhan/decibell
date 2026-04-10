import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import ServerBrowseView from "../features/servers/ServerBrowseView";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import UserPanel from "../features/channels/UserPanel";
import ChatPanel, { ChatHeader } from "../features/chat/ChatPanel";
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
        {activeView === "browse" ? (
          <>
            <DmSidebar />
            <ServerBrowseView />
          </>
        ) : (
          <>
            {/* Sidebar group: DmSidebar + ChannelSidebar + floating UserPanel */}
            <div className="relative flex shrink-0">
              <DmSidebar />
              <ChannelSidebar />
              {/* Floating user panel overlay */}
              <div className="absolute bottom-2 left-2 right-2 z-20">
                <UserPanel />
              </div>
            </div>
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
              <div className="flex min-w-0 flex-1 flex-col">
                <ChatHeader />
                <div className="flex min-h-0 flex-1 overflow-hidden">
                  <ChatPanel hideHeader />
                  {membersPanelVisible && <MembersList />}
                </div>
              </div>
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
