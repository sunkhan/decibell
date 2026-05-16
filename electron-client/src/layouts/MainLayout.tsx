import { useEffect } from "react";
import { invoke } from "../lib/ipc";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerBrowseView from "../features/servers/ServerBrowseView";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import ChatPanel from "../features/chat/ChatPanel";
import FriendsList from "../features/friends/FriendsList";
import MembersList from "../features/friends/MembersList";
import VoicePanel from "../features/voice/VoicePanel";
import UserPanel from "../features/channels/UserPanel";
import UserContextMenu from "../features/voice/UserContextMenu";
import ImageViewer from "../features/chat/ImageViewer";
import SettingsModal from "../features/settings/SettingsModal";
import ImageContextMenu from "../components/ImageContextMenu";
import MembershipRevokedToast from "../features/servers/MembershipRevokedToast";
import MembersAdminPanel from "../features/servers/MembersAdminPanel";
import ChannelSettingsModal from "../features/servers/ChannelSettingsModal";
import InviteModal from "../features/servers/InviteModal";
import DeepLinkJoinModal from "../features/servers/DeepLinkJoinModal";
import PersistentAudioLayer from "../features/chat/PersistentAudioLayer";
import PersistentVideoLayer from "../features/chat/PersistentVideoLayer";
import CrashReportingBanner from "../components/CrashReportingBanner";
import DmChatPanel from "../features/dm/DmChatPanel";
import UserProfilePopup from "../features/dm/UserProfilePopup";
import { useDmEvents } from "../features/dm/useDmEvents";
import { useDragDrop } from "../features/chat/useDragDrop";
import { usePasteToAttach } from "../features/chat/usePasteToAttach";
import { useCentralConnectionStatus } from "../hooks/useCentralConnectionStatus";
import { useWindowTitle } from "../hooks/useWindowTitle";
import { useUiStore } from "../stores/uiStore";

// Mirrors tauri-client/src/layouts/MainLayout.tsx structurally:
//
//   ┌─ ServerBar (horizontal tab strip) ────────────────────────────┐
//   ├─ DmSidebar (left vertical) ─┬─ ChannelSidebar ─┬─ Chat / DM /┤
//   │                              │                  │ Voice /     │
//   │                              │                  │ Browse      │
//   │              [floating UserPanel bottom-left at z-20]         │
//   └────────────────────────────────────────────────────────────────┘
//
// PR4-parity stage defers UserPanel (PR5: voice pipeline), VoicePanel
// (PR5), DmChatPanel (DMs PR), FriendsList / MembersList (friends/
// members PRs), and the modal stack (settings / invites / channel
// settings / image viewer / etc., each landing with its feature PR).
// The structural slots stay open so each PR slots its component in
// without rearranging the shell.
export default function MainLayout() {
  const connectionStatus = useUiStore((s) => s.connectionStatus);
  const activeView = useUiStore((s) => s.activeView);
  const membersPanelVisible = useUiStore((s) => s.membersPanelVisible);
  const dmFriendsPanelVisible = useUiStore((s) => s.dmFriendsPanelVisible);

  // Window-level drag/drop + paste-to-attach hooks. They listen on
  // the window so the user can drop files anywhere over the app.
  useDragDrop();
  usePasteToAttach();
  // DMs flow through the same `message_received` bus event the chat
  // hook reads — useDmEvents filters to context === "dm" and routes
  // into useDmStore.
  useDmEvents();
  // Cross-cutting concerns: the central-server reconnecting banner
  // and the OS window title.
  useCentralConnectionStatus();
  useWindowTitle();

  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 shrink-0 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <CrashReportingBanner />

      <ServerBar />

      <div className="flex flex-1 overflow-hidden">
        {activeView === "browse" ? (
          <>
            <DmSidebar />
            <ServerBrowseView />
          </>
        ) : (
          <>
            {/* Sidebar group: DmSidebar + ChannelSidebar with the
                floating UserPanel anchored bottom-left over them.
                Browse view above renders only DmSidebar — no
                ChannelSidebar — to match tauri-client. */}
            <div className="relative flex shrink-0">
              <DmSidebar />
              <ChannelSidebar />
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
                <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
                  Pick a server from the bar above, or browse to join one.
                </div>
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

      <UserContextMenu />
      <UserProfilePopup />
      <ImageViewer />
      <SettingsModal />
      <ImageContextMenu />
      <MembershipRevokedToast />
      <MembersAdminPanel />
      <ChannelSettingsModal />
      <InviteModal />
      <DeepLinkJoinModal />
      <PersistentAudioLayer />
      <PersistentVideoLayer />
    </div>
  );
}
