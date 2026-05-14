import { memo } from "react";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { UserAvatar } from "../../components/UserAvatar";

interface Props {
  usernames?: string[];
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
    <div className="flex items-center gap-1 rounded bg-error/20 px-1.5 py-0.5">
      <div className="h-1.5 w-1.5 rounded-full bg-error" />
      <span className="text-[9px] font-bold text-error">LIVE</span>
    </div>
  );
}

// Each row subscribes only to the slices it actually displays so a
// speaking-event for one user doesn't re-render every other row.
// Memo'd so identical (props, derived) skip the function call entirely.

interface PresenceRowProps {
  username: string;
  channelId?: string;
  connectedServerId: string | null;
}

const PresenceRow = memo(function PresenceRow({
  username,
  channelId,
  connectedServerId,
}: PresenceRowProps) {
  const isStreaming = useVoiceStore((s) =>
    s.activeStreams.some((st) => st.ownerUsername === username),
  );
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  const userState = useVoiceStore((s) => {
    if (!channelId) return undefined;
    return s.channelUserStates[channelId]?.[username];
  });
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  return (
    <div
      className="group flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-[12px] transition-colors hover:bg-surface-hover"
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
        openContextMenu(username, { x: e.clientX, y: e.clientY });
      }}
    >
      <UserAvatar username={username} size={22} />
      <span className="min-w-0 truncate text-text-secondary transition-colors group-hover:text-text-primary">
        {username}
      </span>
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        {isStreaming && <LiveBadge />}
        {isLocallyMuted && <LocalMuteIcon />}
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
  connectedServerId: string | null;
}

const ActiveRow = memo(function ActiveRow({
  username,
  isLocal,
  rosterMuted,
  rosterDeafened,
  connectedServerId,
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

  const userMuted = isLocal ? selfMuted : rosterMuted;
  const userDeafened = isLocal ? selfDeafened : rosterDeafened;

  return (
    <div
      className="group flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-[12px] transition-colors hover:bg-surface-hover"
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
        openContextMenu(username, { x: e.clientX, y: e.clientY });
      }}
    >
      <div
        className="shrink-0 rounded-md transition-shadow duration-200"
        style={{
          boxShadow: isSpeaking ? "0 0 0 2px #3fb950, 0 0 6px #3fb950" : "none",
        }}
      >
        <UserAvatar username={username} size={22} />
      </div>
      <span
        className={`min-w-0 truncate transition-colors ${
          isSpeaking
            ? "text-[#3fb950]"
            : "text-text-secondary group-hover:text-text-primary"
        }`}
      >
        {username}
      </span>
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        {isStreaming && <LiveBadge />}
        {isLocallyMuted && <LocalMuteIcon />}
        {userDeafened ? <DeafenIcon /> : userMuted ? <MuteIcon /> : null}
      </div>
    </div>
  );
});

export default function VoiceParticipantList({ usernames, channelId }: Props) {
  const participants = useVoiceStore((s) => s.participants);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const localUsername = useAuthStore((s) => s.username);

  if (usernames) {
    if (usernames.length === 0) return null;
    return (
      <div className="space-y-0.5 pb-1 pl-5">
        {usernames.map((u) => (
          <PresenceRow
            key={u}
            username={u}
            channelId={channelId}
            connectedServerId={connectedServerId}
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
          connectedServerId={connectedServerId}
        />
      ))}
    </div>
  );
}
