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
import { useDragDrop } from "../features/chat/useDragDrop";
import { usePasteToAttach } from "../features/chat/usePasteToAttach";
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
                <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
                  Direct messages port with the DMs PR.
                </div>
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
      <ImageViewer />
    </div>
  );
}
