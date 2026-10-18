import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { invoke, listen } from "../../lib/ipc";
import { usePresenceStore } from "../../stores/presenceStore";
import { useAuthStore } from "../../stores/authStore";
import { useAvatarStore } from "../../stores/avatarStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useCallStore } from "../../stores/callStore";
import { useImageViewerStore } from "../../stores/imageViewerStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import type { ServerInfoPayload } from "../../types";
import { stopActiveStream } from "../voice/streaming/StreamCapture";
import { stopRing } from "../call/callActions";
import { clearRetainedImages } from "../chat/imageRetention";
import { abandonDmRequests } from "../chat/historyRequests";
import { requestFriendList } from "../friends/friendList";

export function useAuthEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenSuccess = listen<{ username: string; renames?: boolean; uid?: number | bigint }>(
      "login_succeeded",
      (event) => {
        const uid = Number(event.payload.uid ?? 0) || 0;
        // Central signed the same account in again on its own: a resume, or
        // the password after a refused resume, following a dropped
        // connection.
        const auth = useAuthStore.getState();
        const again = auth.isAuthenticated && uid > 0 && auth.uid === uid;
        auth.login(event.payload.username, !!event.payload.renames, uid);
        // Others' pictures come with their presence; nothing sends our own.
        useAvatarStore.getState().seed(uid);
        // Replace, never push: a history entry is something back/forward
        // could walk to, and the app has no page worth walking back to.
        navigate("/", { replace: true });
        // Pull the server list immediately after login so ServerBar +
        // ServerBrowseView have data to render. Response arrives via
        // `server_list_received` below.
        invoke("request_server_list").catch(() => {});
        // Pull DM conversation previews so ConversationSidebar's cards
        // and the ServerBar's unread-DM tiles are populated before the
        // user opens anything.
        // Response arrives via `dm_conversations_received` (handled
        // in useDmEvents). Failure is silent — same as server list.
        invoke("request_dm_conversations").catch(() => {});
        // What happened while we were away never reached the renderer
        // (review B4). The conversation list is refetched above, as on
        // every sign-in, which resets the unread counts; the loaded
        // conversations reload (the open one catches up in DmChatPanel)
        // and the friend list is asked for again.
        if (again) {
          abandonDmRequests();
          useDmStore.getState().markHistoriesStale();
          requestFriendList();
        }
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
        const chat = useChatStore.getState();
        // Discovery results live in their own list (Discover view). The
        // ServerBar reads `servers` (memberships), so this never touches the
        // user's own tiles.
        chat.setDiscoverableServers(servers);
        // Propagate picture_version so the ServerBar tile / browse
        // view knows up-front whether to lazy-fetch a picture.
        for (const s of event.payload.servers as Array<
          ServerInfoPayload & { pictureVersion?: string }
        >) {
          chat.setServerPictureVersion(String(s.id), s.pictureVersion ?? "");
        }
      },
    );

    const unlistenLoggedOut = listen("logged_out", () => {
      // Native dropped the voice, stream and call engines; the renderer's
      // half goes with them. Otherwise a WebCodecs share keeps capturing
      // (the OS "sharing" indicator stays on) and the next login shows the
      // old voice channel in session (review B10).
      stopActiveStream().catch(() => {});
      stopRing();
      useCallStore.getState().reset();
      useVoiceStore.getState().disconnect();
      // Nothing open carries over to the next account: windows, popups,
      // menus, the image viewer.
      const ui = useUiStore.getState();
      ui.closeModal();
      ui.closeProfilePopup();
      ui.closeContextMenu();
      useImageViewerStore.getState().close();
      useImageContextMenuStore.getState().close();
      // Main's attachment tokens and decryption keys belong to this session.
      window.decibell.attachmentRegistry.clearAll().catch(() => {});
      window.decibell.attachments.clearKeys().catch(() => {});
      useAuthStore.getState().logout();
      useChatStore.getState().resetForLogout();
      usePresenceStore.getState().reset();
      // The next account on this device sees none of this one's DMs,
      // drafts or friends.
      useDmStore.setState({ conversations: {}, activeDmUser: null, pendingDmDeletions: {}, resyncEpoch: 0 });
      useDraftsStore.setState({ channelDrafts: {}, dmDrafts: {}, restored: null, restoredDm: null });
      useFriendsStore.setState({ friends: [], lastActionError: null });
      useUiStore.getState().setActiveView("home");
      useAvatarStore.getState().clearAll();
      clearRetainedImages();
      navigate("/login", { replace: true });
    });

    // AvatarChanged broadcast: server tells us a user just updated
    // their picture. setVersion invalidates the cache when the new
    // version differs from the cached one; the next UserAvatar render
    // for that user will re-fetch.
    const unlistenAvatar = listen<{ username: string; uid?: number | bigint; version: string }>(
      "avatar_changed",
      (event) => {
        useAvatarStore
          .getState()
          .setVersion(Number(event.payload.uid ?? 0) || 0, event.payload.version);
      },
    );

    return () => {
      unlistenSuccess.then((fn) => fn());
      unlistenFailed.then((fn) => fn());
      unlistenRegister.then((fn) => fn());
      unlistenServerList.then((fn) => fn());
      unlistenLoggedOut.then((fn) => fn());
      unlistenAvatar.then((fn) => fn());
    };
  }, [navigate]);
}
