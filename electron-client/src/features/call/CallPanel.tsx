import { memo, useEffect, useState } from "react";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useCallStore } from "../../stores/callStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { UserAvatar } from "../../components/UserAvatar";
import { playSound } from "../../utils/sounds";
import { announceCallStreamStop, endCall, watchCallStream } from "./callActions";

/// In-DM call surface: shown between the DM header and the messages while
/// a call with `peer` is ringing / connecting / active. Voice controls
/// reuse the same native commands as the community voice panel — in a
/// call the VoiceEngine is the same engine, reached peer-to-peer.
export default function CallPanel({ peer }: { peer: string }) {
  const status = useCallStore((s) => s.status);
  const callPeer = useCallStore((s) => s.peer);
  if (status === "idle" || status === "incoming" || callPeer !== peer) return null;
  return <ActiveCallPanel peer={peer} />;
}

function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function ActiveCallPanel({ peer }: { peer: string }) {
  const me = useAuthStore((s) => s.username) ?? "";
  const status = useCallStore((s) => s.status);
  const ringingAcked = useCallStore((s) => s.ringingAcked);
  const startedAt = useCallStore((s) => s.startedAt);
  const connectedPath = useCallStore((s) => s.connectedPath);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const error = useVoiceStore((s) => s.error);
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const [showPicker, setShowPicker] = useState(false);
  const peerStream = activeStreams.find((st) => st.ownerUsername === peer);

  // 1 s tick for the duration readout while active.
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (status !== "active") return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [status]);

  const statusLine =
    status === "outgoing"
      ? ringingAcked
        ? "Ringing…"
        : "Calling…"
      : status === "connecting"
        ? "Connecting…"
        : status === "active" && startedAt
          ? [
              formatDuration(now - startedAt),
              latencyMs != null ? `${latencyMs} ms` : null,
              connectedPath === "host" ? "LAN" : connectedPath === "srflx" ? "direct" : null,
            ]
              .filter(Boolean)
              .join(" · ")
          : "";

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
  const handleHangUp = () => {
    void endCall(status === "outgoing" ? "Cancelled" : "Call ended");
  };
  const handleStopSharing = async () => {
    playSound("stream_stop");
    const { stopActiveStream } = await import("../voice/streaming/StreamCapture");
    await stopActiveStream();
    invoke("stop_screen_share", {}).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
    announceCallStreamStop();
  };

  const live = status === "active";

  return (
    <div className="shrink-0 border-b border-border bg-bg-tertiary px-4 py-4">
      <div className="flex items-center justify-center gap-8">
        <CallTile username={me} muted={isMuted} dim={!live} />
        <CallTile username={peer} muted={false} dim={!live} pulse={status !== "active"} />
      </div>
      <div className="mt-3 text-center font-meta text-[12px] text-text-muted">{statusLine}</div>
      {error && live && (
        <div className="mx-auto mt-2 max-w-md rounded-sm bg-error/10 px-2 py-1 text-center text-[11px] text-error">
          {error}
        </div>
      )}
      {live && (peerStream || isStreaming) && (
        <div className="mt-3 flex items-center justify-center gap-2">
          {peerStream && (
            <button
              onClick={() => void watchCallStream(peer)}
              className="flex h-9 items-center gap-2 rounded-md border border-accent/[0.25] bg-accent/[0.12] px-3 text-[12px] font-medium text-accent transition-colors hover:bg-accent/[0.18]"
              title={`Watch ${peer}'s screen`}
            >
              <span className="flex h-1.5 w-1.5 animate-pulse rounded-full bg-error" />
              Watch {peer}'s screen
              {peerStream.resolutionHeight > 0 && (
                <span className="text-[11px] text-accent/80">
                  {peerStream.resolutionHeight}p{peerStream.fps > 0 ? ` · ${peerStream.fps}fps` : ""}
                </span>
              )}
            </button>
          )}
          {isStreaming && (
            <button
              onClick={() => void watchCallStream(me)}
              className="flex h-9 items-center gap-2 rounded-md border border-border bg-surface-hover px-3 text-[12px] font-medium text-text-secondary transition-colors hover:text-text-bright"
              title="Preview what you're sharing"
            >
              Preview my screen
            </button>
          )}
        </div>
      )}
      <div className="mt-3 flex items-center justify-center gap-2">
        {live && (
          <>
            <CallButton
              title={isMuted ? "Unmute" : "Mute"}
              onClick={handleMute}
              active={isMuted}
            >
              {isMuted ? <MicOffIcon /> : <MicIcon />}
            </CallButton>
            <CallButton
              title={isDeafened ? "Undeafen" : "Deafen"}
              onClick={handleDeafen}
              active={isDeafened}
            >
              {isDeafened ? <DeafenOffIcon /> : <DeafenIcon />}
            </CallButton>
            <CallButton
              title={isStreaming ? "Stop sharing" : "Share your screen"}
              onClick={isStreaming ? () => void handleStopSharing() : () => setShowPicker(true)}
              active={isStreaming}
            >
              {isStreaming ? <StopShareIcon /> : <ShareIcon />}
            </CallButton>
          </>
        )}
        <button
          onClick={handleHangUp}
          title={status === "outgoing" ? "Cancel" : "Hang up"}
          className="flex h-10 items-center gap-2 rounded-full bg-error px-5 text-[13px] font-semibold text-text-bright transition-opacity hover:opacity-90"
        >
          <HangUpIcon />
          {status === "outgoing" ? "Cancel" : "Hang up"}
        </button>
      </div>
      {showPicker && <CaptureSourcePicker onClose={() => setShowPicker(false)} />}
    </div>
  );
}

interface CallTileProps {
  username: string;
  muted: boolean;
  dim: boolean;
  pulse?: boolean;
}

const CallTile = memo(function CallTile({ username, muted, dim, pulse }: CallTileProps) {
  // Per-tile speaking subscription so the other tile's events are no-ops.
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const peerMuted = useVoiceStore(
    (s) => s.participants.find((p) => p.username === username)?.isMuted ?? false,
  );
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const showMuted = muted || peerMuted;
  return (
    <div
      className={`flex cursor-pointer flex-col items-center gap-2 rounded-lg px-4 py-2 transition-all hover:bg-surface-hover ${
        dim ? "opacity-70" : ""
      }`}
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(username, { x: rect.right + 8, y: rect.top }, null);
      }}
    >
      <div className="relative">
        {pulse && <div className="absolute inset-0 animate-ping rounded-lg bg-accent/20" />}
        <div
          className={`relative rounded-lg transition-all duration-150 ${
            isSpeaking
              ? "shadow-[0_0_0_3px_var(--color-bg-tertiary),0_0_0_5px_var(--color-success)]"
              : ""
          }`}
        >
          <UserAvatar username={username} size={64} />
        </div>
        {showMuted && (
          <div className="absolute -bottom-1 -right-1 flex h-[20px] w-[20px] items-center justify-center rounded-full border-[2.5px] border-bg-tertiary bg-bg-light">
            <svg className="h-3 w-3 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <line x1="1" y1="1" x2="23" y2="23" />
              <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
              <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
              <line x1="12" y1="19" x2="12" y2="23" />
              <line x1="8" y1="23" x2="16" y2="23" />
            </svg>
          </div>
        )}
      </div>
      <div className="max-w-[9rem] truncate text-center text-[12px] font-medium text-text-primary">
        {username}
      </div>
    </div>
  );
});

function CallButton({
  title,
  onClick,
  active,
  children,
}: {
  title: string;
  onClick: () => void;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={`flex h-10 w-10 items-center justify-center rounded-full transition-colors ${
        active
          ? "bg-error/15 text-error hover:bg-error/25"
          : "bg-surface-hover text-text-secondary hover:bg-surface-active hover:text-text-bright"
      }`}
    >
      {children}
    </button>
  );
}

function MicIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}
function MicOffIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <line x1="1" y1="1" x2="23" y2="23" />
      <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
      <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}
function DeafenIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
    </svg>
  );
}
function DeafenOffIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}
function HangUpIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91" />
      <line x1="23" y1="1" x2="1" y2="23" />
    </svg>
  );
}
function ShareIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="3" width="20" height="14" rx="2" ry="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </svg>
  );
}
function StopShareIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
      <rect x="8" y="8" width="8" height="8" rx="1" fill="currentColor" stroke="none" />
    </svg>
  );
}
