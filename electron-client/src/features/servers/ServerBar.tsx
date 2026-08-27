import { useEffect, useMemo } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useDmStore, conversationActivityTime } from "../../stores/dmStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
import { UserAvatar } from "../../components/UserAvatar";
import { AVATAR_RADIUS } from "../../components/LetterAvatar";
import { TILE_WIDTH, TILE_HEIGHT } from "./serverTileDimensions";
import type { CommunityServer } from "../../types";

// Tiny 1×1 transparent PNG. Used as the <img> placeholder while the
// picture bytes are in-flight so we don't flash a broken-image icon.
const PLACEHOLDER_DATA_URL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

// Module-level dedupe set for in-flight picture fetches.
// Keyed by "<serverId>:<version>" so a new version triggers a fresh
// fetch even if the previous one is still pending.
const inflightFetches = new Set<string>();

function useFetchServerPictureIfMissing(
  serverId: string,
  version: string,
  cachedDataUrl: string | undefined,
) {
  useEffect(() => {
    if (!version || cachedDataUrl) return;
    const key = `${serverId}:${version}`;
    if (inflightFetches.has(key)) return;
    inflightFetches.add(key);
    invoke("fetch_server_picture", { serverId: parseInt(serverId, 10) })
      .catch(console.error)
      .finally(() => inflightFetches.delete(key));
  }, [serverId, version, cachedDataUrl]);
}

interface ServerTileProps {
  server: CommunityServer;
  isActive: boolean;
  isPending: boolean;
  onClick: (serverId: string) => void;
}

function ServerTile({ server, isActive, isPending, onClick }: ServerTileProps) {
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[server.id] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[server.id]);
  const hasPicture = pictureVersion !== "";

  useFetchServerPictureIfMissing(server.id, pictureVersion, pictureDataUrl);

  const showGlow = isActive && !isPending;

  if (!hasPicture) {
    return (
      <div
        className="relative shrink-0"
        style={{ width: TILE_WIDTH, height: TILE_HEIGHT }}
      >
        {showGlow && <ActiveTileGlow />}
        <button
          onClick={() => !isPending && onClick(server.id)}
          disabled={isPending}
          title={isPending ? "Connecting…" : server.name}
          className={`relative flex h-full w-full items-center gap-2 rounded-md px-3 text-[13px] font-semibold transition-all duration-150 ${
            isPending
              ? "cursor-wait bg-surface-hover text-text-muted opacity-60"
              : isActive
                ? "cursor-pointer bg-accent-mid text-accent-bright"
                : "cursor-pointer text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:-translate-y-px"
          }`}
        >
          {!isPending && isActive && (
            <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
          )}
          <div
            className="flex h-5 w-5 shrink-0 items-center justify-center rounded-sm text-[11px] font-semibold"
            style={{ background: stringToGradient(server.name), color: "var(--color-av-fg)" }}
          >
            {server.name.charAt(0).toUpperCase()}
          </div>
          <span className="min-w-0 flex-1 truncate text-left">{server.name}</span>
        </button>
      </div>
    );
  }

  // Picture branch.
  return (
    <div
      className="relative shrink-0"
      style={{ width: TILE_WIDTH, height: TILE_HEIGHT }}
    >
      {showGlow && <ActiveTileGlow />}
      <button
        onClick={() => !isPending && onClick(server.id)}
        disabled={isPending}
        title={isPending ? "Connecting…" : server.name}
        className={`relative flex h-full w-full items-center justify-center overflow-hidden rounded-md px-3 transition-all duration-150 ${
          isPending
            ? "cursor-wait opacity-60"
            : isActive
              ? "cursor-pointer"
              : "cursor-pointer hover:-translate-y-px"
        }`}
      >
        <img
          src={pictureDataUrl ?? PLACEHOLDER_DATA_URL}
          alt={server.name}
          className="absolute inset-0 h-full w-full object-cover"
        />
        {/* Dim overlay only when inactive */}
        {!isActive && <div className="absolute inset-0 bg-black/45" />}
        {/* Name overlay when inactive. Image fills the rectangle via
            object-cover; tile width is now fixed via TILE_WIDTH, so the
            name doesn't need to drive sizing. */}
        {!isActive && (
          <span className="relative max-w-full truncate text-[13px] font-semibold text-white">
            {server.name}
          </span>
        )}
        {!isPending && isActive && (
          <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
        )}
      </button>
    </div>
  );
}

// GPU-composited replacement for the dropPulse box-shadow animation
// that used to live directly on the active tile. Sits as a sibling of
// the button inside a relative wrapper so the picture branch's
// overflow-hidden doesn't clip the glow. Carries the original
// keyframe's peak box-shadow values as static styling; the `breathe`
// keyframe (defined in globals.css) only varies opacity, which
// Chromium auto-promotes to a composited layer — so the shadow is
// rasterized once and the per-frame cost collapses to an alpha blend
// on the GPU. Replaces the ~3-5 % idle CPU draw of the original.
function ActiveTileGlow() {
  return (
    <div
      className="pointer-events-none absolute inset-0 rounded-md shadow-[0_0_0_1.5px_color-mix(in_srgb,var(--color-accent)_60%,transparent),0_0_22px_4px_color-mix(in_srgb,var(--color-accent)_22%,transparent)] animate-[breathe_2.4s_ease-in-out_infinite] will-change-[opacity]"
    />
  );
}

// Unread DMs surface here, between the home button and the server
// tabs: one avatar tile per peer with unreadCount > 0, most recent
// activity first (leftmost = newest). A tile lives exactly as long as
// the conversation is unread — DmChatPanel's mark-read effect zeroes
// unreadCount as soon as the conversation is on screen, so clicking a
// tile (which opens the conversation) is what makes it disappear.
// This replaced the vertical DM rail (2026-08-27), which listed every
// conversation permanently; the full list is ConversationSidebar on
// the home / dm views. No store state of its own — it is a filtered
// view of dmStore.conversations.
function UnreadDmTiles() {
  const conversations = useDmStore((s) => s.conversations);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const unread = useMemo(
    () =>
      Object.values(conversations)
        .filter((c) => c.unreadCount > 0)
        .sort((a, b) => conversationActivityTime(b) - conversationActivityTime(a)),
    [conversations],
  );
  if (unread.length === 0) return null;

  return (
    <>
      {unread.map((conv) => {
        const count = conv.unreadCount > 99 ? "99+" : String(conv.unreadCount);
        // Radius matches the avatar inside so the hover lift and the
        // badge cutout stay concentric with it (see LetterAvatar).
        return (
          <button
            key={conv.username}
            onClick={() => {
              setActiveDmUser(conv.username);
              setActiveView("dm");
            }}
            title={`${conv.username} — ${count} unread`}
            className="relative shrink-0 cursor-pointer transition-transform duration-150 animate-[dropIn_150ms_ease-out] hover:-translate-y-px"
            style={{ borderRadius: AVATAR_RADIUS }}
          >
            <UserAvatar username={conv.username} size={TILE_HEIGHT} />
            <div className="absolute -top-1 -right-1 flex h-[18px] min-w-[18px] items-center justify-center rounded-full border-[2px] border-bg-darkest bg-error px-1 text-[10px] font-semibold leading-none text-white">
              {count}
            </div>
          </button>
        );
      })}
      <div className="mx-1 h-6 w-px shrink-0 bg-border-divider" />
    </>
  );
}

// Horizontal server tab strip — one tab per server the user is
// currently connected to OR auto-rejoining as part of post-login
// fanout. Pending tiles render as "connecting…" until the matching
// community_auth_responded lands (success → flips to normal, failure
// → drops + toast via useServerEvents). The home button (left)
// toggles the home view; the add button (right) opens
// ServerBrowseView. Unread-DM tiles sit between the two (see
// UnreadDmTiles). Servers in `servers` that are neither connected
// nor pending live only in ServerBrowseView. Leaving a server is
// only accessible via the ServerChannelsSidebar dropdown — no
// inline affordance on the tile itself.
export default function ServerBar() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const pendingMembershipServerIds = useChatStore(
    (s) => s.pendingMembershipServerIds,
  );
  const activeServerId = useChatStore((s) => s.activeServerId);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const visible = useMemo(
    () =>
      servers.filter(
        (s) =>
          connectedServers.has(s.id) || pendingMembershipServerIds.has(s.id),
      ),
    [servers, connectedServers, pendingMembershipServerIds],
  );

  const handleServerClick = (serverId: string) => {
    const currentChannel = useChatStore.getState().activeChannelId;
    setActiveServer(serverId);
    setActiveView("server");
    const channels = useChatStore.getState().channelsByServer[serverId] ?? [];
    const currentInThisServer = channels.some((ch) => ch.id === currentChannel);
    if (!currentInThisServer) {
      setActiveChannel(null);
      const firstText = channels.find((ch) => ch.type === "text");
      if (firstText) {
        setActiveChannel(firstText.id);
      }
    }
  };

  return (
    <div className="chrome-scope relative z-10 flex h-[58px] shrink-0 items-center bg-bg-darkest">
      {/* No bottom separator: the chrome gutter under the bar is the
          separation from the workspace panel. The home button's left edge
          lines up with the panel's (both 8px in from the window edge). */}
      <div className="flex shrink-0 items-center px-2">
        {/* Colour-only states: no ring when active and no hover lift, so
            the button never changes size or position. */}
        <button
          onClick={() => { setActiveServer(null); setActiveChannel(null); setActiveView("home"); }}
          className={`flex h-[40px] w-[40px] shrink-0 items-center justify-center rounded-sm transition-colors duration-150 ${
            activeView === "home"
              ? "bg-accent text-on-accent"
              : "bg-surface-active text-text-secondary hover:bg-accent hover:text-on-accent"
          }`}
          title="Home"
        >
          <svg className="h-[20px] w-[20px]" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 3l-9.5 8.5c-.3.27-.15.5.25.5H5v8a1 1 0 001 1h4v-5.5a1 1 0 011-1h2a1 1 0 011 1V21h4a1 1 0 001-1v-8h2.25c.4 0 .55-.23.25-.5L12 3z" />
          </svg>
        </button>
      </div>

      <div className="h-7 w-px shrink-0 bg-border-divider" />

      {/* Unread DMs, then server tabs */}
      <div className="flex flex-1 items-center gap-2 px-2">
        <UnreadDmTiles />
        {visible.map((server) => (
          <ServerTile
            key={server.id}
            server={server}
            // Active state is only meaningful when the user is actually
            // *viewing* this server (the channel grid or a voice
            // channel inside it). On home / browse / dm views the
            // activeServerId is sticky so the user's previously
            // selected server doesn't get forgotten — but we must NOT
            // render the tile as active in those modes, or the breathing
            // glow + image-only treatment lies about where the user is.
            isActive={
              activeServerId === server.id &&
              (activeView === "server" || activeView === "voice")
            }
            isPending={pendingMembershipServerIds.has(server.id)}
            onClick={handleServerClick}
          />
        ))}

        {visible.length > 0 && (
          <div className="mx-1 h-6 w-px shrink-0 bg-border-divider" />
        )}

        <button
          onClick={() => setActiveView("browse")}
          className={`flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-md text-lg transition-all duration-150 ${
            activeView === "browse"
              ? "bg-success text-on-accent"
              : "border-[1.5px] border-dashed border-text-muted text-text-muted hover:border-accent hover:bg-accent-soft hover:text-accent"
          }`}
          title="Browse servers"
        >
          <svg className="h-[18px] w-[18px]" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <line x1="12" y1="5" x2="12" y2="19" />
            <line x1="5" y1="12" x2="19" y2="12" />
          </svg>
        </button>
      </div>
    </div>
  );
}
