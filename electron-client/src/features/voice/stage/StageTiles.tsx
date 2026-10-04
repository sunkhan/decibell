import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useVoiceStore } from "../../../stores/voiceStore";
import { useUiStore } from "../../../stores/uiStore";
import { useCodecSettingsStore } from "../../../stores/codecSettingsStore";
import { useDisplayName } from "../../../hooks/useDisplayName";
import { useMenuPosition, type MenuAnchor } from "../../../hooks/useMenuPosition";
import { UserAvatar } from "../../../components/UserAvatar";
import { AVATAR_RADIUS } from "../../../components/LetterAvatar";
import { avatarColor } from "../../../utils/colors";
import { canWatchStream } from "../../../utils/canWatchStream";
import { VideoCodec, type StreamInfo } from "../../../types";
import StreamVideoPlayer from "../StreamVideoPlayer";
import { CODEC_COLOR, formatResolution } from "../CodecBadge";
import { getStreamPipHost, placeStreamPip } from "../streamPipHost";
import { TILE_ASPECT } from "./stageLayout";
import { focusStream, stopWatchingStream, switchToStream, watchStreamToo } from "./streamActions";
import {
  CloseIcon,
  ExpandIcon,
  EyeIcon,
  HeadphonesOffIcon,
  LockIcon,
  MicOffIcon,
  PlayIcon,
  PlusIcon,
  SpeakerIcon,
} from "./icons";

// The voice stage's tiles. Every tile is 16:9 at a width the stage computes;
// each subscribes to its own slices (speaking, thumbnail, …) so an event for
// one user re-renders one tile, never the grid.
//
// Two surface families:
//   participant tiles sit on the theme (bg-light washed with the user's
//     avatar colour, theme chip, theme text);
//   stream tiles are video, so everything over them is a fixed dark scrim
//     with white type in every theme — same rule as CodecBadge.

/// Fixed dark scrim chip for anything drawn over video. The base carries no
/// size, so callers pick one (SCRIM = tile chips, SCRIM_LG = the focused
/// view) instead of stacking two conflicting height utilities.
export const SCRIM_BASE =
  "flex items-center whitespace-nowrap rounded-sm border border-white/10 bg-black/70 font-meta text-meta font-medium text-white";
export const SCRIM = `${SCRIM_BASE} h-6 gap-1.5 px-2`;
export const SCRIM_LG = `${SCRIM_BASE} h-8 gap-2 px-2.5`;

function shortCodec(codec: VideoCodec): string | null {
  switch (codec) {
    case VideoCodec.AV1:
      return "AV1";
    case VideoCodec.H265:
      return "HEVC";
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW:
      return "H.264";
    default:
      return null;
  }
}

/// Avatar size for a tile height: ~38% of it, on an 8px step, 32–96.
function avatarFor(height: number): number {
  return Math.min(96, Math.max(32, Math.round((height * 0.38) / 8) * 8));
}

function openProfile(e: React.SyntheticEvent<HTMLElement>, username: string, serverId: string | null) {
  const rect = e.currentTarget.getBoundingClientRect();
  useUiStore.getState().openProfilePopup(username, { x: rect.right + 8, y: rect.top }, serverId);
}

function openMenu(e: React.MouseEvent, username: string, serverId: string | null) {
  e.preventDefault();
  useUiStore.getState().openContextMenu(username, { x: e.clientX, y: e.clientY }, serverId);
}

/// Enter / Space activate a div-with-role=button like a real button.
function onActivateKey(e: React.KeyboardEvent, run: () => void) {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    run();
  }
}

const LIVE_SIZES = { sm: "h-4 px-1", md: "h-5 px-1.5", lg: "h-8 px-2.5" } as const;

export function LivePill({
  small,
  size = small ? "sm" : "md",
  className,
}: {
  small?: boolean;
  size?: keyof typeof LIVE_SIZES;
  className?: string;
}) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-sm bg-live font-meta text-micro font-bold uppercase leading-none tracking-wider text-white ${LIVE_SIZES[size]} ${className ?? ""}`}
    >
      <span className="h-1.5 w-1.5 rounded-full bg-white" />
      Live
    </span>
  );
}

/// "1080p60 · AV1" (+ lock when the streamer pinned the codec), on a scrim.
export function QualityPill({
  stream,
  large,
  className,
}: {
  stream: StreamInfo;
  large?: boolean;
  className?: string;
}) {
  const codec = shortCodec(stream.currentCodec);
  const hasRes = stream.resolutionWidth > 0 && stream.resolutionHeight > 0 && stream.fps > 0;
  if (!codec && !hasRes) return null;
  return (
    <span
      className={`${large ? SCRIM_LG : SCRIM} ${className ?? ""}`}
      title={stream.enforcedCodec ? `Stream locked to ${codec}` : undefined}
    >
      {hasRes && (
        <span className="text-white/70">
          {formatResolution(stream.resolutionWidth, stream.resolutionHeight)}
          {stream.fps}
        </span>
      )}
      {codec && <span style={{ color: CODEC_COLOR[stream.currentCodec] }}>{codec}</span>}
      {stream.enforcedCodec !== VideoCodec.UNKNOWN && (
        <span style={{ color: "#e0b050" }}>
          <LockIcon size={11} bold />
        </span>
      )}
    </span>
  );
}

// ── participant tile ─────────────────────────────────────────────

interface ParticipantTileProps {
  username: string;
  isLocal: boolean;
  /// Roster snapshot; our own mute/deafen comes from the store toggles.
  rosterMuted: boolean;
  rosterDeafened: boolean;
  serverMuted?: boolean;
  serverDeafened?: boolean;
  connectedServerId: string | null;
  width: number;
  /// Filmstrip size: tighter chip, no "you" tag.
  mini?: boolean;
}

export const ParticipantTile = memo(function ParticipantTile({
  username,
  isLocal,
  rosterMuted,
  rosterDeafened,
  serverMuted,
  serverDeafened,
  connectedServerId,
  width,
  mini,
}: ParticipantTileProps) {
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const isStreaming = useVoiceStore((s) => s.activeStreams.some((st) => st.ownerUsername === username));
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  const selfMuted = useVoiceStore((s) => isLocal && s.isMuted);
  const selfDeafened = useVoiceStore((s) => isLocal && s.isDeafened);
  const displayName = useDisplayName(connectedServerId, username);

  const muted = isLocal ? selfMuted : rosterMuted;
  const deafened = isLocal ? selfDeafened : rosterDeafened;
  const height = width / TILE_ASPECT;
  const icon = mini ? 11 : 14;
  // Small tiles lift the avatar so it clears the name chip.
  const lift = height < 140 ? (mini ? "pb-3.5" : "pb-5") : "";

  return (
    <div
      role="button"
      tabIndex={0}
      title={displayName}
      onClick={(e) => openProfile(e, username, connectedServerId)}
      onContextMenu={(e) => openMenu(e, username, connectedServerId)}
      onKeyDown={(e) => onActivateKey(e, () => openProfile(e, username, connectedServerId))}
      // Speaking flips the outline instantly (no transition): a fading
      // ring repaints for its whole duration on every VAD flip.
      className={`group relative flex aspect-video shrink-0 items-center justify-center overflow-hidden border border-border shadow-raised ${
        mini ? "rounded-md" : "rounded-lg"
      } ${isSpeaking ? "outline-2 -outline-offset-2 outline-success" : ""} ${lift}`}
      style={{
        width,
        background: `color-mix(in srgb, ${avatarColor(username)} var(--tile-tint), var(--color-bg-light))`,
      }}
    >
      <span aria-hidden className="pointer-events-none absolute inset-0 group-hover:bg-surface-hover" />
      <div
        className={`relative ${isSpeaking ? "ring-4 ring-success/25" : ""}`}
        style={{ borderRadius: AVATAR_RADIUS }}
      >
        <UserAvatar username={username} size={avatarFor(height)} />
      </div>
      {/* Not in the filmstrip: the stream itself sits right beside it there. */}
      {isStreaming && !mini && <LivePill className="absolute left-2 top-2" />}
      <div
        className={`absolute flex items-center rounded-sm bg-bg-mid/80 font-channel font-medium text-text-primary ${
          mini
            ? "bottom-1 left-1 h-4.5 max-w-[calc(100%-0.5rem)] gap-1 px-1.5 text-micro"
            : "bottom-2 left-2 h-6.5 max-w-[calc(100%-1rem)] gap-1.5 px-2 text-meta"
        }`}
      >
        <span className="truncate">{displayName}</span>
        {isLocal && !mini && <span className="font-normal text-text-muted">you</span>}
        {(serverMuted || serverDeafened) && (
          <span
            title={serverDeafened ? "Server deafened by a moderator" : "Server muted by a moderator"}
            className="flex h-4 w-4 shrink-0 items-center justify-center rounded-sm bg-error text-on-error"
          >
            {serverDeafened ? <HeadphonesOffIcon size={10} bold /> : <MicOffIcon size={10} bold />}
          </span>
        )}
        {deafened ? (
          <span title="Deafened" className="text-error">
            <HeadphonesOffIcon size={icon} bold />
          </span>
        ) : muted ? (
          <span title="Muted" className="text-error">
            <MicOffIcon size={icon} bold />
          </span>
        ) : null}
        {isLocallyMuted && (
          <span title="Muted by you" className="text-accent">
            <SpeakerIcon muted size={icon} bold />
          </span>
        )}
      </div>
    </div>
  );
});

// ── stream tile ──────────────────────────────────────────────────

interface StreamTileProps {
  stream: StreamInfo;
  isWatching: boolean;
  /// This stream is the one the persistent player (StreamPipManager) holds.
  isPip: boolean;
  isOwnStream: boolean;
  /// Some other stream is already being watched → the hover offers
  /// Switch / Watch too instead of a single Watch.
  watchingOthers: boolean;
  connectedServerId: string | null;
  width: number;
  /// Filmstrip size: no hover actions (a click focuses), small pills.
  mini?: boolean;
}

/// Primary actions over a video tile: solid white / outlined white.
const ACTION_PRIMARY =
  "inline-flex h-9 items-center gap-2 rounded-md bg-white px-3.5 text-[13px] font-semibold text-black hover:bg-white/90";
const ACTION_SECONDARY =
  "inline-flex h-9 items-center gap-2 rounded-md border border-white/30 bg-white/15 px-3.5 text-[13px] font-semibold text-white hover:bg-white/25";

export const StreamTile = memo(function StreamTile({
  stream,
  isWatching,
  isPip,
  isOwnStream,
  watchingOthers,
  connectedServerId,
  width,
  mini,
}: StreamTileProps) {
  const owner = stream.ownerUsername;
  const thumbnail = useVoiceStore((s) => s.streamThumbnails[owner]);
  const displayName = useDisplayName(connectedServerId, owner);
  const decodeCaps = useCodecSettingsStore.getState().decodeCaps;
  const { canWatch, reason } = isOwnStream ? { canWatch: true, reason: undefined } : canWatchStream(stream, decodeCaps);
  const label = isOwnStream ? "Your screen" : `${displayName}'s screen`;

  // The stream the persistent player already decodes is shown by moving that
  // player's host in here (as the full view and the mini player do) rather
  // than starting a second decoder for the same stream.
  const slotRef = useRef<HTMLDivElement>(null);
  const hostsPip = isWatching && isPip;
  useLayoutEffect(() => {
    const slot = slotRef.current;
    if (!hostsPip || !slot) return;
    placeStreamPip(slot);
    return () => {
      const host = getStreamPipHost();
      if (host.parentElement === slot) host.remove();
    };
  }, [hostsPip]);

  // A click on the tile itself runs its first hover action. In the
  // filmstrip that's always a non-destructive focus.
  const primary = () => {
    if (!canWatch) return;
    if (mini || isWatching || isOwnStream || !watchingOthers) focusStream(owner);
    else switchToStream(owner);
  };
  const act = (run: () => void) => (e: React.MouseEvent) => {
    e.stopPropagation();
    run();
  };

  return (
    <div
      role="button"
      tabIndex={canWatch ? 0 : -1}
      aria-disabled={!canWatch}
      aria-label={label}
      title={reason ?? label}
      onClick={primary}
      onKeyDown={(e) => onActivateKey(e, primary)}
      className={`group relative aspect-video shrink-0 overflow-hidden border border-border bg-black shadow-raised ${
        mini ? "rounded-md" : "rounded-lg"
      } ${canWatch ? "" : "cursor-not-allowed"} ${isWatching ? "outline-2 -outline-offset-2 outline-accent" : ""}`}
      style={{ width }}
    >
      <div className="absolute inset-0 flex items-center justify-center">
        {hostsPip ? (
          <div ref={slotRef} className="h-full w-full" />
        ) : isWatching ? (
          <StreamVideoPlayer streamerUsername={owner} className="h-full w-full object-cover" />
        ) : thumbnail ? (
          <img src={thumbnail} alt="" className="h-full w-full object-cover" draggable={false} />
        ) : (
          <UserAvatar username={owner} size={mini ? 28 : 56} />
        )}
      </div>

      {/* Hover actions / can't-play notice sit under the corner pills, so the
          labels stay legible while the tile is dimmed. */}
      {!canWatch ? (
        <div className="absolute inset-0 flex items-center justify-center bg-black/55">
          <span className={SCRIM}>
            <LockIcon size={12} bold />
            {mini ? "Unsupported" : `Can't play ${shortCodec(stream.enforcedCodec || stream.currentCodec) ?? "this codec"}`}
          </span>
        </div>
      ) : (
        !mini && (
          <div className="absolute inset-0 flex items-center justify-center gap-2 bg-black/45 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100">
            {isWatching ? (
              <>
                <button type="button" className={ACTION_PRIMARY} onClick={act(() => focusStream(owner))}>
                  <ExpandIcon size={14} />
                  Focus
                </button>
                <button type="button" className={ACTION_SECONDARY} onClick={act(() => stopWatchingStream(owner))}>
                  <CloseIcon size={14} bold />
                  Stop
                </button>
              </>
            ) : isOwnStream ? (
              <button type="button" className={ACTION_PRIMARY} onClick={act(() => focusStream(owner))}>
                <PlayIcon />
                Preview
              </button>
            ) : watchingOthers ? (
              <>
                <button
                  type="button"
                  className={ACTION_PRIMARY}
                  title="Stop the streams you're watching and watch this one"
                  onClick={act(() => switchToStream(owner))}
                >
                  <PlayIcon />
                  Switch
                </button>
                <button
                  type="button"
                  className={ACTION_SECONDARY}
                  title="Keep what you're watching and add this stream"
                  onClick={act(() => watchStreamToo(owner))}
                >
                  <PlusIcon size={14} bold />
                  Watch too
                </button>
              </>
            ) : (
              <button type="button" className={ACTION_PRIMARY} onClick={act(() => focusStream(owner))}>
                <PlayIcon />
                Watch stream
              </button>
            )}
          </div>
        )
      )}

      <div className={`absolute flex gap-1.5 ${mini ? "left-1 top-1" : "left-2 top-2"}`}>
        <LivePill small={mini} />
        {isWatching && !mini && (
          <span className="flex h-5 items-center gap-1 rounded-sm bg-accent px-1.5 font-meta text-micro font-semibold leading-none text-on-accent">
            <EyeIcon size={12} bold />
            Watching
          </span>
        )}
      </div>

      {mini ? (
        <div className="absolute bottom-1 left-1 flex h-4.5 max-w-[calc(100%-0.5rem)] items-center rounded-sm bg-black/70 px-1.5 font-channel text-micro font-medium text-white">
          <span className="truncate">{label}</span>
        </div>
      ) : (
        <>
          <QualityPill stream={stream} className="absolute right-2 top-2" />
          <div className="absolute bottom-2 left-2 flex max-w-[calc(100%-4.5rem)]">
            <span className={`${SCRIM} min-w-0`}>
              <UserAvatar username={owner} size={16} />
              <span className="truncate">{label}</span>
              {stream.hasAudio && (
                <span className="text-white/70" title="Has audio">
                  <SpeakerIcon size={13} />
                </span>
              )}
            </span>
          </div>
          {stream.watcherCount > 0 && (
            <span className={`${SCRIM} absolute bottom-2 right-2`} title={`${stream.watcherCount} watching`}>
              <EyeIcon size={13} />
              {stream.watcherCount}
            </span>
          )}
        </>
      )}

    </div>
  );
});

// ── "+N more" overflow ───────────────────────────────────────────

interface OverflowTileProps {
  hidden: readonly string[];
  width: number;
  mini?: boolean;
  connectedServerId: string | null;
  channelName: string;
}

/// Stands in for the people who don't fit: a few stacked avatars and the
/// count. Click → the full list. Lights up while someone in it talks (they
/// swap into view a moment later, see useVisibleParticipants).
export const OverflowTile = memo(function OverflowTile({
  hidden,
  width,
  mini,
  connectedServerId,
  channelName,
}: OverflowTileProps) {
  const [anchor, setAnchor] = useState<MenuAnchor | null>(null);
  const anySpeaking = useVoiceStore((s) => hidden.some((u) => s.speakingUsers.has(u)));
  const tileRef = useRef<HTMLDivElement>(null);
  const toggle = () => {
    const el = tileRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    // Anchored at the tile's right edge: where the list doesn't fit to the
    // right (the usual case — "+N" is the last tile) it flips to end flush
    // with the tile.
    setAnchor((a) => (a ? null : { x: r.right, y: r.top - 8 }));
  };
  const height = width / TILE_ASPECT;
  const stackSize = mini ? 20 : height < 110 ? 24 : 28;

  return (
    <>
      <div
        ref={tileRef}
        role="button"
        tabIndex={0}
        aria-expanded={anchor != null}
        title={`${hidden.length} more in ${channelName}`}
        onClick={toggle}
        onKeyDown={(e) => onActivateKey(e, toggle)}
        className={`group relative flex aspect-video shrink-0 flex-col items-center justify-center gap-2 overflow-hidden border border-border bg-bg-light shadow-raised ${
          mini ? "rounded-md" : "rounded-lg"
        } ${anchor ? "outline-2 -outline-offset-2 outline-accent" : anySpeaking ? "outline-2 -outline-offset-2 outline-success" : ""}`}
        style={{ width }}
      >
        <span aria-hidden className="pointer-events-none absolute inset-0 group-hover:bg-surface-hover" />
        <div className="flex">
          {hidden.slice(0, 3).map((u, i) => (
            <div
              key={u}
              className={`ring-2 ring-bg-light ${i > 0 ? "-ml-2" : ""}`}
              style={{ borderRadius: AVATAR_RADIUS }}
            >
              <UserAvatar username={u} size={stackSize} />
            </div>
          ))}
        </div>
        <div className="flex items-baseline gap-1">
          <span className={`font-channel font-emphasis text-text-bright ${mini ? "text-micro" : "text-member"}`}>
            +{hidden.length}
          </span>
          {!mini && <span className="font-meta text-micro text-text-muted">more</span>}
        </div>
      </div>
      {anchor &&
        createPortal(
          <OverflowPopover
            anchor={anchor}
            hidden={hidden}
            connectedServerId={connectedServerId}
            channelName={channelName}
            tileRef={tileRef}
            onClose={() => setAnchor(null)}
          />,
          document.body,
        )}
    </>
  );
});

function OverflowPopover({
  anchor,
  hidden,
  connectedServerId,
  channelName,
  tileRef,
  onClose,
}: {
  anchor: MenuAnchor;
  hidden: readonly string[];
  connectedServerId: string | null;
  channelName: string;
  tileRef: React.RefObject<HTMLDivElement | null>;
  onClose: () => void;
}) {
  const { ref, style } = useMenuPosition<HTMLDivElement>(anchor, { prefer: "above" });

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      // The tile toggles itself; anything else outside closes.
      if (ref.current?.contains(t) || tileRef.current?.contains(t)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [ref, tileRef, onClose]);

  return (
    <div
      ref={ref}
      style={style}
      className="z-50 w-70 rounded-lg border border-border bg-bg-light p-1.5 shadow-float"
    >
      <div className="px-2 pb-1.5 pt-2 font-meta text-section font-medium uppercase leading-none tracking-section text-text-muted">
        {hidden.length} more in {channelName}
      </div>
      {hidden.map((u) => (
        <HiddenRow key={u} username={u} connectedServerId={connectedServerId} onPick={onClose} />
      ))}
    </div>
  );
}

const HiddenRow = memo(function HiddenRow({
  username,
  connectedServerId,
  onPick,
}: {
  username: string;
  connectedServerId: string | null;
  onPick: () => void;
}) {
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const p = useVoiceStore((s) => s.participants.find((x) => x.username === username));
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  const displayName = useDisplayName(connectedServerId, username);
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={(e) => {
        openProfile(e, username, connectedServerId);
        onPick();
      }}
      onContextMenu={(e) => openMenu(e, username, connectedServerId)}
      onKeyDown={(e) =>
        onActivateKey(e, () => {
          openProfile(e, username, connectedServerId);
          onPick();
        })
      }
      className="group flex h-9 items-center gap-2.5 rounded-sm px-2 font-channel text-member hover:bg-surface-hover"
    >
      <div
        className={`shrink-0 ${isSpeaking ? "ring-2 ring-success" : ""}`}
        style={{ borderRadius: AVATAR_RADIUS }}
      >
        <UserAvatar username={username} size={24} />
      </div>
      <span
        className={`min-w-0 truncate ${isSpeaking ? "text-success" : "text-text-secondary group-hover:text-text-primary"}`}
      >
        {displayName}
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-1.5">
        {(p?.isServerMuted || p?.isServerDeafened) && (
          <span className="flex h-4 w-4 items-center justify-center rounded-sm bg-error text-on-error">
            {p.isServerDeafened ? <HeadphonesOffIcon size={10} bold /> : <MicOffIcon size={10} bold />}
          </span>
        )}
        {p?.isDeafened ? (
          <span className="text-error">
            <HeadphonesOffIcon size={14} bold />
          </span>
        ) : p?.isMuted ? (
          <span className="text-error">
            <MicOffIcon size={14} bold />
          </span>
        ) : null}
        {isLocallyMuted && (
          <span className="text-accent">
            <SpeakerIcon muted size={14} bold />
          </span>
        )}
      </span>
    </div>
  );
});
