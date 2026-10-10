import { memo } from "react";
import { useVoiceStore, voiceKey } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useDisplayName } from "../../hooks/useDisplayName";
import { UserAvatar } from "../../components/UserAvatar";
import { PERM, useChannelPermission } from "../servers/permissions";
import { onMemberDragEnd, onMemberDragStart } from "./memberDrag";

/// `usernames` (+ the channel's `serverId` / `channelId`) renders another
/// channel's roster from presence; without it, the connected channel's
/// live participants.
interface Props {
  usernames?: string[];
  serverId?: string;
  channelId?: string;
}

function MuteIcon() {
  return (
    <svg className="h-3.5 w-3.5 shrink-0 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <line x1="1" y1="1" x2="23" y2="23" />
      <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
      <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}

function LocalMuteIcon() {
  return (
    <svg className="h-3.5 w-3.5 shrink-0 text-accent" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <title>Muted by you</title>
      <path d="M11 5L6 9H2v6h4l5 4V5z" />
      <line x1="23" y1="9" x2="17" y2="15" />
      <line x1="17" y1="9" x2="23" y2="15" />
    </svg>
  );
}

/// Moderator-applied mute/deafen (persisted on the member). Distinct
/// from the user's own mute so people can tell "muted themselves" from
/// "muted by a mod".
/// Moderator mute / deafen, or no SPEAK permission in this channel
/// (`suppressed`, permissions v3) — the server drops their audio either way.
function ServerMuteBadge({ deafened, suppressed }: { deafened: boolean; suppressed?: boolean }) {
  const title = deafened
    ? "Server deafened by a moderator"
    : suppressed
      ? "Can't speak in this channel"
      : "Server muted by a moderator";
  return (
    <span
      title={title}
      className="rounded-sm bg-error/15 px-1 py-px text-[9px] font-semibold uppercase tracking-[0.04em] text-error"
    >
      {deafened ? "Srv deaf" : suppressed ? "No speak" : "Srv mute"}
    </span>
  );
}

function DeafenIcon() {
  return (
    <svg className="h-3.5 w-3.5 shrink-0 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}

function LiveBadge() {
  return (
    <div className="flex items-center gap-1 rounded-sm bg-error/20 px-1.5 py-0.5">
      <div className="h-1.5 w-1.5 rounded-full bg-error" />
      <span className="text-[10px] font-semibold text-error">LIVE</span>
    </div>
  );
}

// Each row subscribes only to the slices it actually displays so a
// speaking-event for one user doesn't re-render every other row.
// Memo'd so identical (props, derived) skip the function call entirely.

interface PresenceRowProps {
  username: string;
  serverId: string;
  channelId: string;
  /// We hold MOVE_MEMBERS here: drag the row onto another voice channel.
  draggable: boolean;
}

/// A member of a channel we may not be in — possibly on no voice session
/// at all, or one on another server — so everything keys on the row's
/// own server: nickname, profile, the context menu's moderation, LIVE.
const PresenceRow = memo(function PresenceRow({
  username,
  serverId,
  channelId,
  draggable,
}: PresenceRowProps) {
  const isStreaming = useVoiceStore((s) => {
    const loc = s.streamsByUser.get(username);
    return !!loc && loc.serverId === serverId && loc.channelId === channelId;
  });
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  const userState = useVoiceStore((s) => s.channelUserStates[voiceKey(serverId, channelId)]?.[username]);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const displayName = useDisplayName(serverId, username);

  return (
    <div
      className="group flex cursor-pointer items-center gap-2 rounded-sm px-1.5 py-1 text-member transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(
          username,
          { x: rect.right + 8, y: rect.top },
          serverId,
        );
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY }, serverId);
      }}
      draggable={draggable}
      onDragStart={draggable ? (e) => onMemberDragStart(e, serverId, channelId, username) : undefined}
      onDragEnd={draggable ? onMemberDragEnd : undefined}
    >
      <UserAvatar username={username} size={22} />
      <span className="min-w-0 truncate text-text-secondary transition-colors group-hover:text-text-primary">
        {displayName}
      </span>
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        {isStreaming && <LiveBadge />}
        {isLocallyMuted && <LocalMuteIcon />}
        {(userState?.isServerMuted || userState?.isServerDeafened || userState?.isSuppressed) && (
          <ServerMuteBadge
            deafened={!!userState?.isServerDeafened}
            suppressed={!userState?.isServerMuted && !!userState?.isSuppressed}
          />
        )}
        {userState?.isDeafened ? <DeafenIcon /> : userState?.isMuted ? <MuteIcon /> : null}
      </div>
    </div>
  );
});

interface ActiveRowProps {
  username: string;
  isLocal: boolean;
  /// Snapshot props from the parent list (only change on roster updates,
  /// which already re-render the parent). Live state — speaking, stream,
  /// local-mute, our own mute/deafen — is fetched per-row below.
  rosterMuted: boolean;
  rosterDeafened: boolean;
  serverMuted?: boolean;
  serverDeafened?: boolean;
  /// No SPEAK in this channel (permissions v3).
  suppressed?: boolean;
  connectedServerId: string | null;
  connectedChannelId: string | null;
  /// We hold MOVE_MEMBERS here and it isn't us: drag onto another channel.
  draggable: boolean;
}

const ActiveRow = memo(function ActiveRow({
  username,
  isLocal,
  rosterMuted,
  rosterDeafened,
  serverMuted,
  serverDeafened,
  suppressed,
  connectedServerId,
  connectedChannelId,
  draggable,
}: ActiveRowProps) {
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const isStreaming = useVoiceStore((s) =>
    s.activeStreams.some((st) => st.ownerUsername === username),
  );
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  // Local user's mute/deafen comes from the top-level toggle, not the
  // roster — subscribe directly so our own indicator updates instantly.
  const selfMuted = useVoiceStore((s) => s.isMuted);
  const selfDeafened = useVoiceStore((s) => s.isDeafened);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const displayName = useDisplayName(connectedServerId, username);

  const userMuted = isLocal ? selfMuted : rosterMuted;
  const userDeafened = isLocal ? selfDeafened : rosterDeafened;

  return (
    <div
      className="group flex cursor-pointer items-center gap-2 rounded-sm px-1.5 py-1 text-member transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(
          username,
          { x: rect.right + 8, y: rect.top },
          connectedServerId,
        );
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY }, connectedServerId);
      }}
      draggable={draggable}
      onDragStart={
        draggable && connectedServerId && connectedChannelId
          ? (e) => onMemberDragStart(e, connectedServerId, connectedChannelId, username)
          : undefined
      }
      onDragEnd={draggable ? onMemberDragEnd : undefined}
    >
      {/* Ring and name colour flip instantly (no transition), like
          UserPanel's ring: a 150 ms box-shadow / colour transition on every
          speaking flip repaints continuously once a few people talk. */}
      <div
        className="shrink-0 rounded-sm"
        style={{
          boxShadow: isSpeaking ? "0 0 0 2px var(--color-success), 0 0 6px var(--color-success)" : "none",
        }}
      >
        <UserAvatar username={username} size={22} />
      </div>
      <span
        className={`min-w-0 truncate ${
          isSpeaking
            ? "text-success"
            : "text-text-secondary group-hover:text-text-primary"
        }`}
      >
        {displayName}
      </span>
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        {isStreaming && <LiveBadge />}
        {isLocallyMuted && <LocalMuteIcon />}
        {(serverMuted || serverDeafened || suppressed) && (
          <ServerMuteBadge deafened={!!serverDeafened} suppressed={!serverMuted && !!suppressed} />
        )}
        {userDeafened ? <DeafenIcon /> : userMuted ? <MuteIcon /> : null}
      </div>
    </div>
  );
});

export default function VoiceParticipantList({ usernames, serverId, channelId }: Props) {
  const participants = useVoiceStore((s) => s.participants);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const localUsername = useAuthStore((s) => s.username);
  // Drag-to-move (memberDrag.ts): MOVE_MEMBERS in the channel these rows
  // are in. The hierarchy is checked when a drag starts.
  const canMoveHere = useChannelPermission(
    usernames ? serverId ?? null : connectedServerId,
    usernames ? channelId ?? null : connectedChannelId,
    PERM.MOVE_MEMBERS,
  );

  if (usernames) {
    if (usernames.length === 0 || !serverId || !channelId) return null;
    return (
      <div className="space-y-0.5 pb-1 pl-5">
        {usernames.map((u) => (
          <PresenceRow
            key={u}
            username={u}
            serverId={serverId}
            channelId={channelId}
            draggable={canMoveHere && u !== localUsername}
          />
        ))}
      </div>
    );
  }

  if (participants.length === 0) return null;

  return (
    <div className="space-y-0.5 pb-1 pl-5">
      {participants.map((p) => (
        <ActiveRow
          key={p.username}
          username={p.username}
          isLocal={p.username === localUsername}
          rosterMuted={p.isMuted}
          rosterDeafened={p.isDeafened}
          serverMuted={p.isServerMuted}
          serverDeafened={p.isServerDeafened}
          suppressed={p.isSuppressed}
          connectedServerId={connectedServerId}
          connectedChannelId={connectedChannelId}
          draggable={canMoveHere && p.username !== localUsername}
        />
      ))}
    </div>
  );
}
