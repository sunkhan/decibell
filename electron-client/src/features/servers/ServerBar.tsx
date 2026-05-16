import { useEffect, useMemo } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
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
          className={`relative flex h-full w-full items-center gap-2 rounded-lg px-3 text-[13px] font-semibold transition-all duration-200 ${
            isPending
              ? "cursor-wait bg-surface-hover text-text-muted opacity-60"
              : isActive
                ? "bg-accent-mid text-accent-bright"
                : "text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:-translate-y-px"
          }`}
        >
          {!isPending && isActive && (
            <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
          )}
          <div
            className="flex h-5 w-5 shrink-0 items-center justify-center rounded-[5px] text-[11px] font-semibold text-white"
            style={{ background: stringToGradient(server.name) }}
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
        className={`relative flex h-full w-full items-center justify-center overflow-hidden rounded-lg px-3 transition-all duration-200 ${
          isPending
            ? "cursor-wait opacity-60"
            : isActive
              ? ""
              : "hover:-translate-y-px"
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
      className="pointer-events-none absolute inset-0 rounded-lg shadow-[0_0_0_1.5px_rgba(56,143,255,0.60),0_0_22px_4px_rgba(56,143,255,0.22)] animate-[breathe_2.4s_ease-in-out_infinite] will-change-[opacity]"
    />
  );
}

// Horizontal server tab strip — one tab per server the user is
// currently connected to OR auto-rejoining as part of post-login
// fanout. Pending tiles render as "connecting…" until the matching
// community_auth_responded lands (success → flips to normal, failure
// → drops + toast via useServerEvents). The home button (left)
// toggles the home view; the add button (right) opens
// ServerBrowseView. Servers in `servers` that are neither connected
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
    <div className="relative z-10 flex h-[58px] shrink-0 items-center bg-bg-darkest">
      {/* Bottom separator starts after the home-button column. */}
      <div className="pointer-events-none absolute bottom-0 left-[68px] right-0 border-b border-border" />
      {/* Home button — width matches DM sidebar */}
      <div className="flex w-[68px] shrink-0 items-center justify-center">
        <button
          onClick={() => { setActiveServer(null); setActiveChannel(null); setActiveView("home"); }}
          className={`flex h-[40px] w-[40px] shrink-0 items-center justify-center rounded-md transition-all duration-200 ${
            activeView === "home"
              ? "bg-accent text-white shadow-[0_0_0_2px_var(--color-accent)]"
              : "bg-surface-active text-text-secondary hover:bg-accent hover:text-white hover:-translate-y-0.5"
          }`}
          title="Home"
        >
          <svg className="h-[20px] w-[20px]" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 3l-9.5 8.5c-.3.27-.15.5.25.5H5v8a1 1 0 001 1h4v-5.5a1 1 0 011-1h2a1 1 0 011 1V21h4a1 1 0 001-1v-8h2.25c.4 0 .55-.23.25-.5L12 3z" />
          </svg>
        </button>
      </div>

      <div className="h-7 w-px shrink-0 bg-border-divider" />

      {/* Server tabs */}
      <div className="flex flex-1 items-center gap-2 px-2">
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
          className={`flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-lg text-lg transition-all duration-200 ${
            activeView === "browse"
              ? "bg-success text-white"
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
