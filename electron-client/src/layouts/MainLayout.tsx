import { useEffect } from "react";
import { invoke } from "../lib/ipc";
import ServerBar from "../features/servers/ServerBar";
import ServerBrowseView from "../features/servers/ServerBrowseView";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import ChatPanel from "../features/chat/ChatPanel";
import FriendsList from "../features/friends/FriendsList";
import FriendsPage from "../features/friends/FriendsPage";
import MembersList from "../features/friends/MembersList";
import VoicePanel from "../features/voice/VoicePanel";
import MiniStreamPlayer from "../features/voice/MiniStreamPlayer";
import StreamPipManager from "../features/voice/StreamPipManager";
import UserPanel from "../features/channels/UserPanel";
import UserContextMenu from "../features/voice/UserContextMenu";
import ImageViewer from "../features/chat/ImageViewer";
import SettingsModal from "../features/settings/SettingsModal";
import ImageContextMenu from "../components/ImageContextMenu";
import MembershipRevokedToast from "../features/servers/MembershipRevokedToast";
import ChannelSettingsModal from "../features/servers/ChannelSettingsModal";
import InviteModal from "../features/servers/InviteModal";
import DeepLinkJoinModal from "../features/servers/DeepLinkJoinModal";
import PersistentAudioLayer from "../features/chat/PersistentAudioLayer";
import PersistentVideoLayer from "../features/chat/PersistentVideoLayer";
import CrashReportingBanner from "../components/CrashReportingBanner";
import DmChatPanel from "../features/dm/DmChatPanel";
import UserProfilePopup from "../features/dm/UserProfilePopup";
import { useDmEvents } from "../features/dm/useDmEvents";
import { useCallEvents } from "../features/call/useCallEvents";
import IncomingCallModal from "../features/call/IncomingCallModal";
import { useE2eeEvents } from "../features/e2ee/useE2eeEvents";
import PassphraseModal from "../features/e2ee/PassphraseModal";
import { useDragDrop } from "../features/chat/useDragDrop";
import { usePasteToAttach } from "../features/chat/usePasteToAttach";
import { useCentralConnectionStatus } from "../hooks/useCentralConnectionStatus";
import { useWindowTitle } from "../hooks/useWindowTitle";
import { useUiStore } from "../stores/uiStore";

// Mirrors tauri-client/src/layouts/MainLayout.tsx structurally, minus
// the vertical DM rail (removed 2026-08-27: unread DMs surface as avatar
// tiles in the ServerBar instead, and the full conversation list is the
// ConversationSidebar on the home / dm views):
//
//   ┌─ ServerBar (home · unread DMs · server tabs) ─────────────────┐
//   │ ╭─ ChannelSidebar ─┬─ Chat / DM / Voice / Browse ───────────╮ │
//   │ │                  │                                        │ │
//   │ │ [floating UserPanel bottom-left at z-20]                  │ │
//   │ ╰────────────────────────────────────────────────────────────╯ │
//   └────────────────────────────────────────────────────────────────┘
//
// The workspace floats in the chrome: an 8px chrome-toned gutter on the
// left / right / bottom (the ServerBar's own bottom padding is the top
// one) and the sidebar + main area sit inside as a rounded, bordered
// panel — so the frameless window edge reads as a frame, not a cut.
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
  // P2P DM calls: central call-config + CALL_SIGNAL relay listener.
  useCallEvents();
  // E2EE DMs: status pushes from native (drives the unlock prompt and
  // the post-unlock history reload) + peer key-change notices.
  useE2eeEvents();
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

      {/* `chrome-ground` paints the gutter from the chrome palette as a
          background only. Not `chrome-scope`: that re-scopes --color-*
          for the whole subtree and would drag the chat canvas into the
          chrome ramp in `console-split`. And not a backdrop with a
          negative z-index: that needs a stacking context on this root,
          which would flatten the image viewer / profile popup under the
          AppLayout-level toasts. */}
      <div className="chrome-ground flex min-h-0 flex-1 px-2 pb-2">
        <div
          className="flex min-w-0 flex-1 overflow-hidden rounded-lg border border-border"
          data-pip-content-row
        >
          {activeView === "browse" ? (
            <ServerBrowseView />
          ) : (
            <>
              {/* Sidebar group: ChannelSidebar with the floating
                  UserPanel anchored bottom-left over it. Browse view
                  above renders no sidebar at all. */}
              {/* chrome-scope: in `console-split` this whole group —
                  channel sidebar + floating user/voice panel — paints
                  from the dark `console` palette while the chat canvas
                  beside it stays on `console-light`. Inert in the other
                  four themes. */}
              <div className="chrome-scope relative flex shrink-0">
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
                // Home gives everything right of the DM list to Friends.
                // FriendsList — the 260px rail — stays for the `dm` view,
                // where it sits beside an open conversation.
                <FriendsPage />
              ) : (
                <>
                  <ChatPanel />
                  {membersPanelVisible && <MembersList />}
                </>
              )}
            </>
          )}
        </div>
      </div>

      {/* Owns the single persistent stream player, portaled into a host node
          that the full view and the mini player reparent between themselves —
          so the decoder is never torn down when switching views. */}
      <StreamPipManager />
      {/* Floating pop-out stream player — shows the watched stream while the
          user is in any non-voice view so it keeps playing as they browse. */}
      <MiniStreamPlayer />

      <UserContextMenu />
      <UserProfilePopup />
      <ImageViewer />
      <SettingsModal />
      <ImageContextMenu />
      <MembershipRevokedToast />
      <ChannelSettingsModal />
      <InviteModal />
      <DeepLinkJoinModal />
      <IncomingCallModal />
      <PassphraseModal />
      <PersistentAudioLayer />
      <PersistentVideoLayer />
    </div>
  );
}
