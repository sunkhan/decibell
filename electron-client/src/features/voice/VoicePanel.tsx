import { useMemo, useRef, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { useDisplayName } from "../../hooks/useDisplayName";
import { playSound } from "../../utils/sounds";
import type { StreamInfo, VoiceParticipant } from "../../types";
import StreamViewPanel from "./StreamViewPanel";
import CaptureSourcePicker from "./CaptureSourcePicker";
import { StreamAudioButton } from "./StreamAudioPopover";
import { useStreamThumbnails } from "./useStreamThumbnails";
import { PERM, useChannelPermission, usePermission } from "../servers/permissions";
import { LockGlyph } from "../chat/MessageBubble";
import {
  MAX_TILE_W,
  MIN_TILE_W,
  ROW_TILE_W,
  TILE_ASPECT,
  TILE_GAP,
  fitGrid,
  gridCapacity,
  rowCapacity,
  useElementSize,
  useVisibleParticipants,
} from "./stage/stageLayout";
import { OverflowTile, ParticipantTile, StreamTile } from "./stage/StageTiles";
import {
  ChevronRightIcon,
  HeadphonesIcon,
  HeadphonesOffIcon,
  LeaveIcon,
  MicIcon,
  MicOffIcon,
  ScreenIcon,
  SignalIcon,
  SpeakerIcon,
  StopShareIcon,
  UserPlusIcon,
  UsersIcon,
} from "./stage/icons";

/// The community voice view (design: the "Voice View Redesign" canvas,
/// 2026-10-04 — tiled stage + floating dock, shared with the DM CallStage).
///
///   alone    → your tile, large, with an invite / share prompt
///   grid     → everyone as a 16:9 tile, best-fit to the stage; past the
///              tile-size floor the tail collapses into "+N more" and hidden
///              speakers swap into view (stage/stageLayout.ts)
///   streams  → live streams large on top, people in one compact row below
///   focused  → StreamViewPanel: the stream takes the stage, filmstrip below
///
/// The dock floats over the bottom of every mode except window fullscreen
/// (StreamViewPanel draws its own overlay controls there).

const EMPTY_CHANNELS: never[] = [];
/// Gap between the streams block and the people row under it.
const SECTION_GAP = 16;
/// Live streams can grow bigger than people — they're the content.
const MAX_STREAM_W = 960;
/// Below this the streams block stops shrinking and the stage scrolls.
const MIN_STREAMS_H = 180;

export default function VoicePanel() {
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  // Permissions v2: STREAM is resolved per channel by the server.
  const canStream = useChannelPermission(connectedServerId, connectedChannelId, PERM.STREAM);
  const participants = useVoiceStore((s) => s.participants);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  // No top-level speakingUsers, latencyMs (3 s ping) or streamThumbnails
  // (one per unwatched stream every 3 s) subscriptions — the tiles,
  // HeaderStats and the visibility hook each subscribe to their own slice,
  // so those events don't re-render the whole panel.
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const pipStream = useVoiceStore((s) => s.pipStream);
  const isStreamFullscreen = useVoiceStore((s) => s.isStreamFullscreen);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
  });
  const ownUsername = useAuthStore((s) => s.username);

  const [showPicker, setShowPicker] = useState(false);

  useStreamThumbnails();

  const channelName = channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  return (
    <div className="relative flex min-h-0 flex-1 flex-col bg-bg-mid">
      {!isStreamFullscreen && (
        <VoiceHeader
          channelName={channelName}
          focused={fullscreenStream}
          ownUsername={ownUsername}
          connectedServerId={connectedServerId}
          participantCount={participants.length}
        />
      )}

      {watchingStreams.length > 0 && (
        <div className={fullscreenStream ? "flex min-h-0 flex-1" : "hidden"}>
          <StreamViewPanel />
        </div>
      )}

      {!fullscreenStream && (
        // Unmount (not `hidden`) the stage while a stream is focused: a
        // display:none inline StreamVideoPlayer keeps its VideoDecoder
        // running, so every watched tile decoded off-screen behind the
        // focused one.
        <VoiceStage
          participants={participants}
          activeStreams={activeStreams}
          watchingStreams={watchingStreams}
          pipStream={pipStream}
          ownUsername={ownUsername}
          connectedServerId={connectedServerId}
          channelName={channelName}
          canStream={canStream}
          onShare={() => setShowPicker(true)}
        />
      )}

      {!isStreamFullscreen && <VoiceDock canStream={canStream} onShare={() => setShowPicker(true)} />}

      {showPicker && connectedServerId && connectedChannelId && (
        <CaptureSourcePicker
          serverId={connectedServerId}
          channelId={connectedChannelId}
          onClose={() => setShowPicker(false)}
        />
      )}
    </div>
  );
}

// ── header ───────────────────────────────────────────────────────

const HEADER_TITLE = "font-channel text-title font-emphasis tracking-title text-text-bright";

function VoiceHeader({
  channelName,
  focused,
  ownUsername,
  connectedServerId,
  participantCount,
}: {
  channelName: string;
  focused: string | null;
  ownUsername: string | null;
  connectedServerId: string | null;
  participantCount: number;
}) {
  const focusedName = useDisplayName(connectedServerId, focused ?? "");
  return (
    <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border-divider px-4">
      <SpeakerIcon size={18} className="text-text-muted" />
      {focused ? (
        // Breadcrumb while a stream is focused: the channel name is the way
        // back to the grid (as is Esc and the video's grid button).
        <>
          <button
            type="button"
            onClick={() => useVoiceStore.getState().setFullscreenStream(null)}
            title="Back to the grid (Esc)"
            className="-mx-1 shrink-0 rounded-sm px-1 font-channel text-title font-medium tracking-title text-text-secondary hover:bg-surface-hover hover:text-text-primary"
          >
            {channelName}
          </button>
          <ChevronRightIcon size={14} className="-mx-1 text-text-muted" />
          <span className={`truncate ${HEADER_TITLE}`}>
            {focused === ownUsername ? "Your screen" : `${focusedName}'s screen`}
          </span>
        </>
      ) : (
        <span className={`truncate ${HEADER_TITLE}`}>{channelName}</span>
      )}
      <VoiceEncryptionBadge />
      <HeaderStats participantCount={participantCount} />
    </div>
  );
}

/// Header right side: participant count + voice ping. Owns the latencyMs
/// subscription so the 3 s ping only re-renders this.
function HeaderStats({ participantCount }: { participantCount: number }) {
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const tone =
    latencyMs == null ? "" : latencyMs <= 70 ? "text-success" : latencyMs < 175 ? "text-warning" : "text-error";
  return (
    <div className="ml-auto flex shrink-0 items-center gap-1 font-meta text-meta tabular-nums text-text-muted">
      <span className="flex h-7 items-center gap-1.5 px-2" title={`${participantCount} in voice`}>
        <UsersIcon size={14} />
        {participantCount}
      </span>
      {latencyMs != null && (
        <>
          <span className="h-4 w-px bg-border-divider" />
          <span className={`flex h-7 items-center gap-1.5 px-2 ${tone}`} title="Voice latency">
            <SignalIcon />
            {latencyMs} ms
          </span>
        </>
      )}
    </div>
  );
}

/// MLS state of the connected channel: sealed and verified, still joining
/// the group, resyncing after a missed epoch, or quarantined because a
/// member's identity didn't verify (nothing is sent until they're removed).
function VoiceEncryptionBadge() {
  const e2ee = useVoiceStore((s) => s.e2ee);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  if (!connectedChannelId) return null;
  const state = e2ee?.state ?? "joining";
  const { label, tone, title } =
    state === "ready"
      ? {
          label: "Encrypted",
          tone: "bg-success/15 text-success",
          title: `End-to-end encrypted (MLS epoch ${e2ee?.epoch ?? 0}, ${e2ee?.members ?? 0} members)`,
        }
      : state === "quarantine"
        ? {
            label: "Unverified member",
            tone: "bg-warning/15 text-warning",
            title: `Not sending: ${e2ee?.unverified.join(", ")} could not be verified`,
          }
        : state === "failed"
          ? { label: "Encryption failed", tone: "bg-error/15 text-error", title: "Leave and rejoin the channel" }
          : { label: "Securing…", tone: "bg-text-muted/15 text-text-muted", title: "Joining the channel's encryption group" };
  return (
    <span
      title={title}
      className={`flex h-5.5 shrink-0 items-center gap-1.5 rounded-sm px-2 font-meta text-micro font-medium ${tone}`}
    >
      <LockGlyph size={11} />
      {label}
    </span>
  );
}

// ── stage ────────────────────────────────────────────────────────

interface VoiceStageProps {
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  watchingStreams: string[];
  pipStream: string | null;
  ownUsername: string | null;
  connectedServerId: string | null;
  channelName: string;
  canStream: boolean;
  onShare: () => void;
}

function VoiceStage({
  participants,
  activeStreams,
  watchingStreams,
  pipStream,
  ownUsername,
  connectedServerId,
  channelName,
  canStream,
  onShare,
}: VoiceStageProps) {
  const ref = useRef<HTMLDivElement>(null);
  const { width, height } = useElementSize(ref);
  const measured = width > 0 && height > 0;

  // Keyed on the joined names so a mute flip (a fresh participants array)
  // doesn't hand the visibility hook a "new" roster.
  const rosterKey = participants.map((p) => p.username).join("\n");
  const roster = useMemo(() => (rosterKey ? rosterKey.split("\n") : []), [rosterKey]);
  const streamersKey = activeStreams.map((s) => s.ownerUsername).join("\n");
  const pinned = useMemo(() => {
    const set = new Set(streamersKey ? streamersKey.split("\n") : []);
    if (ownUsername) set.add(ownUsername);
    return set;
  }, [streamersKey, ownUsername]);
  const byName = useMemo(() => new Map(participants.map((p) => [p.username, p])), [participants]);

  const hasStreams = activeStreams.length > 0;
  const n = roster.length;

  // Slots for people, and the tile sizes, for the current mode.
  let slots = n;
  let peopleW = MIN_TILE_W;
  let peopleCols = 1;
  let streams = { cols: 1, tileW: 0 };
  if (measured && hasStreams) {
    const rowCap = rowCapacity(width, ROW_TILE_W, TILE_GAP);
    if (n > rowCap) slots = rowCap - 1;
    peopleW = ROW_TILE_W;
    peopleCols = rowCap;
    const streamsH = Math.max(MIN_STREAMS_H, height - ROW_TILE_W / TILE_ASPECT - SECTION_GAP);
    streams = fitGrid(activeStreams.length, width, streamsH, TILE_GAP, MAX_STREAM_W);
  } else if (measured && n > 1) {
    const cap = gridCapacity(width, height, TILE_GAP, MIN_TILE_W);
    if (n > cap) slots = cap - 1;
    const fit = fitGrid(Math.min(n, cap), width, height, TILE_GAP, MAX_TILE_W);
    peopleW = fit.tileW;
    peopleCols = fit.cols;
  }
  const overflow = slots < n;
  const visible = useVisibleParticipants(roster, slots, pinned);
  const hidden = useMemo(() => {
    if (!overflow) return [];
    const shown = new Set(visible);
    return roster.filter((u) => !shown.has(u));
  }, [overflow, visible, roster]);

  const rowMax = (cols: number, w: number) => cols * w + (cols - 1) * TILE_GAP;

  const peopleTiles = (
    <>
      {visible.map((u) => {
        const p = byName.get(u);
        return (
          <ParticipantTile
            key={u}
            username={u}
            isLocal={u === ownUsername}
            rosterMuted={p?.isMuted ?? false}
            rosterDeafened={p?.isDeafened ?? false}
            serverMuted={p?.isServerMuted}
            serverDeafened={p?.isServerDeafened}
            connectedServerId={connectedServerId}
            width={peopleW}
          />
        );
      })}
      {overflow && (
        <OverflowTile
          hidden={hidden}
          width={peopleW}
          connectedServerId={connectedServerId}
          channelName={channelName}
        />
      )}
    </>
  );

  let body: React.ReactNode = null;
  if (!measured) {
    body = null;
  } else if (!hasStreams && n <= 1) {
    body = (
      <AloneStage
        username={roster[0] ?? ownUsername ?? ""}
        participant={participants[0]}
        isLocal={!roster[0] || roster[0] === ownUsername}
        width={width}
        height={height}
        channelName={channelName}
        connectedServerId={connectedServerId}
        canStream={canStream}
        onShare={onShare}
      />
    );
  } else if (hasStreams) {
    body = (
      <div className="flex flex-col items-center justify-center gap-4" style={{ minHeight: height }}>
        <div className="flex flex-wrap justify-center gap-3" style={{ maxWidth: rowMax(streams.cols, streams.tileW) }}>
          {activeStreams.map((stream) => (
            <StreamTile
              key={stream.streamId}
              stream={stream}
              isWatching={watchingStreams.includes(stream.ownerUsername)}
              isPip={stream.ownerUsername === pipStream}
              isOwnStream={stream.ownerUsername === ownUsername}
              watchingOthers={watchingStreams.some((u) => u !== stream.ownerUsername)}
              connectedServerId={connectedServerId}
              width={streams.tileW}
            />
          ))}
        </div>
        <div className="flex flex-wrap justify-center gap-3" style={{ maxWidth: rowMax(peopleCols, peopleW) }}>
          {peopleTiles}
        </div>
      </div>
    );
  } else {
    body = (
      <div className="flex items-center justify-center" style={{ minHeight: height }}>
        <div className="flex flex-wrap justify-center gap-3" style={{ maxWidth: rowMax(peopleCols, peopleW) }}>
          {peopleTiles}
        </div>
      </div>
    );
  }

  return (
    // pb-24 keeps every tile clear of the floating dock.
    <div ref={ref} className="relative min-h-0 flex-1 overflow-y-auto px-6 pb-24 pt-6">
      {body}
    </div>
  );
}

/// Only you in the channel: your tile, large, and a nudge to invite people
/// or start sharing.
function AloneStage({
  username,
  participant,
  isLocal,
  width,
  height,
  channelName,
  connectedServerId,
  canStream,
  onShare,
}: {
  username: string;
  participant: VoiceParticipant | undefined;
  isLocal: boolean;
  width: number;
  height: number;
  channelName: string;
  connectedServerId: string | null;
  canStream: boolean;
  onShare: () => void;
}) {
  const canManageInvites = usePermission(connectedServerId, PERM.MANAGE_INVITES);
  // InviteModal manages the *active* server's invites.
  const activeServerId = useChatStore((s) => s.activeServerId);
  const serverName = useChatStore(
    (s) => s.servers.find((sv) => sv.id === connectedServerId)?.name ?? "the server",
  );
  const canInvite = canManageInvites && activeServerId === connectedServerId;
  // Room for the copy + buttons under the tile.
  const tileW = Math.max(160, Math.min(480, width, Math.floor((height - 140) * TILE_ASPECT)));
  const copy = canInvite
    ? `Invite people to ${serverName}${canStream ? ", or share your screen while you wait." : "."}`
    : canStream
      ? "Share your screen while you wait for others."
      : "People show up here as they join.";

  if (!username) return null;
  return (
    <div className="flex flex-col items-center justify-center gap-5.5" style={{ minHeight: height }}>
      <ParticipantTile
        username={username}
        isLocal={isLocal}
        rosterMuted={participant?.isMuted ?? false}
        rosterDeafened={participant?.isDeafened ?? false}
        serverMuted={participant?.isServerMuted}
        serverDeafened={participant?.isServerDeafened}
        connectedServerId={connectedServerId}
        width={tileW}
      />
      <div className="flex flex-col items-center gap-1.5 text-center">
        <div className={HEADER_TITLE}>It's just you in {channelName}</div>
        <div className="text-[13px] text-text-muted">{copy}</div>
      </div>
      {(canInvite || canStream) && (
        <div className="flex gap-2">
          {canInvite && (
            <button
              type="button"
              onClick={() => useUiStore.getState().openModal("invite-manage")}
              className="flex items-center gap-2 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
            >
              <UserPlusIcon size={16} />
              Invite people
            </button>
          )}
          {canStream && (
            <button
              type="button"
              onClick={onShare}
              className="flex items-center gap-2 rounded-sm bg-surface-hover px-4 py-2 text-[13px] font-semibold text-text-primary hover:bg-surface-active"
            >
              <ScreenIcon size={16} />
              Share screen
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// ── dock ─────────────────────────────────────────────────────────

const DOCK_BUTTON = "flex h-10 w-10 items-center justify-center rounded-md";
const DOCK_TONES = {
  soft: "bg-surface-hover text-text-secondary hover:bg-surface-active hover:text-text-bright",
  danger: "bg-error/15 text-error hover:bg-error/25",
  on: "bg-accent text-on-accent hover:bg-accent-hover",
} as const;

function DockButton({
  title,
  tone,
  onClick,
  children,
}: {
  title: string;
  tone: keyof typeof DOCK_TONES;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button type="button" title={title} aria-label={title} onClick={onClick} className={`${DOCK_BUTTON} ${DOCK_TONES[tone]}`}>
      {children}
    </button>
  );
}

function DockDivider() {
  return <div className="mx-0.5 h-6 w-px bg-border-divider" />;
}

/// The floating control dock: your mic and headphones, screen share (+ the
/// stream-audio app picker while live), leave. Same shape as CallStage's.
function VoiceDock({ canStream, onShare }: { canStream: boolean; onShare: () => void }) {
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const isStreaming = useVoiceStore((s) => s.isStreaming);

  // Sounds match UserPanel's controls for the same actions.
  const handleMute = () => {
    if (isDeafened) {
      playSound("undeafen");
      invoke("set_voice_deafen", { deafened: false }).catch(console.error);
      invoke("set_voice_mute", { muted: false }).catch(console.error);
    } else {
      playSound(isMuted ? "unmute" : "mute");
      invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
    }
  };

  const handleDeafen = () => {
    playSound(isDeafened ? "undeafen" : "deafen");
    invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
  };

  const handleStopSharing = async () => {
    playSound("stream_stop");
    const { connectedServerId, connectedChannelId } = useVoiceStore.getState();
    // Tear down the renderer-side capture + encoder first so no more
    // frames are pushed to native after we tell native to stop.
    const { stopActiveStream } = await import("./streaming/StreamCapture");
    await stopActiveStream();
    invoke("stop_screen_share", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
    }).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
  };

  const handleDisconnect = async () => {
    playSound("disconnect");
    const v = useVoiceStore.getState();
    const { connectedServerId, connectedChannelId } = v;
    // If we're streaming, stop the capture/encoder and tell native to
    // stop BEFORE leaving. Otherwise capture keeps running and, since
    // disconnect() hides the dock, there's no UI left to end it.
    if (v.isStreaming) {
      const { stopActiveStream } = await import("./streaming/StreamCapture");
      await stopActiveStream();
      if (connectedServerId && connectedChannelId) {
        await invoke("stop_screen_share", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
        }).catch(console.error);
      }
      useVoiceStore.getState().setIsStreaming(false);
    }
    if (connectedServerId && connectedChannelId) {
      // Best-effort, un-awaited: leave_voice_channel below drops all watch
      // subscriptions server-side, so don't serialize N round-trips into the
      // disconnect path.
      const own = useAuthStore.getState().username;
      for (const username of useVoiceStore.getState().watchingStreams) {
        if (username !== own) {
          invoke("stop_watching", {
            serverId: connectedServerId,
            channelId: connectedChannelId,
            targetUsername: username,
          }).catch(() => {});
        }
      }
    }
    invoke("leave_voice_channel").catch(console.error);
    useVoiceStore.getState().disconnect();
    useUiStore.getState().setActiveView("server");
  };

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-4 z-10 flex justify-center">
      <div className="pointer-events-auto flex items-center gap-1.5 rounded-lg border border-border bg-bg-light p-1.5 shadow-float">
        <DockButton title={isMuted ? "Unmute" : "Mute"} tone={isMuted ? "danger" : "soft"} onClick={handleMute}>
          {isMuted ? <MicOffIcon /> : <MicIcon />}
        </DockButton>
        <DockButton title={isDeafened ? "Undeafen" : "Deafen"} tone={isDeafened ? "danger" : "soft"} onClick={handleDeafen}>
          {isDeafened ? <HeadphonesOffIcon /> : <HeadphonesIcon />}
        </DockButton>
        {(canStream || isStreaming) && (
          <>
            <DockDivider />
            <DockButton
              title={isStreaming ? "Stop sharing" : "Share your screen"}
              tone={isStreaming ? "on" : "soft"}
              onClick={isStreaming ? () => void handleStopSharing() : onShare}
            >
              {isStreaming ? <StopShareIcon /> : <ScreenIcon />}
            </DockButton>
            <StreamAudioButton size={18} className={`${DOCK_BUTTON} ${DOCK_TONES.soft}`} />
          </>
        )}
        <DockDivider />
        <button
          type="button"
          title="Disconnect"
          aria-label="Disconnect"
          onClick={() => void handleDisconnect()}
          className="flex h-10 w-14 items-center justify-center rounded-md bg-error text-on-error hover:bg-error/85"
        >
          <LeaveIcon />
        </button>
      </div>
    </div>
  );
}
