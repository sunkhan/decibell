import { useEffect, useRef, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToGradient } from "../../utils/colors";

const MIN_DB = -40;
const MAX_DB = 15;
const DEFAULT_DB = 0;

/** Convert dB to linear gain: 10^(dB/20) */
function dbToGain(db: number): number {
  if (db <= MIN_DB) return 0;
  return Math.pow(10, db / 20);
}

/** Format dB for display */
function formatDb(db: number): string {
  if (db <= MIN_DB) return "Muted";
  if (db === 0) return "0 dB";
  return `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`;
}

/** Percentage label (0 dB = 100%) */
function dbToPercent(db: number): string {
  if (db <= MIN_DB) return "0%";
  const pct = Math.round(dbToGain(db) * 100);
  return `${pct}%`;
}

export default function UserContextMenu() {
  const username = useUiStore((s) => s.contextMenuUser);
  const anchor = useUiStore((s) => s.contextMenuAnchor);
  const closeContextMenu = useUiStore((s) => s.closeContextMenu);
  const userVolumes = useVoiceStore((s) => s.userVolumes);
  const setUserVolume = useVoiceStore((s) => s.setUserVolume);
  const localMutedUsers = useVoiceStore((s) => s.localMutedUsers);
  const toggleLocalMute = useVoiceStore((s) => s.toggleLocalMute);
  const currentUsername = useAuthStore((s) => s.username);
  const menuRef = useRef<HTMLDivElement>(null);
  const isLocallyMuted = username ? localMutedUsers.has(username) : false;

  const currentDb = username ? (userVolumes[username] ?? DEFAULT_DB) : DEFAULT_DB;

  const handleVolumeChange = useCallback(
    (db: number) => {
      if (!username) return;
      const clamped = Math.max(MIN_DB, Math.min(MAX_DB, db));
      setUserVolume(username, clamped);
      // If locally muted, save the dB value but keep gain at 0
      if (!localMutedUsers.has(username)) {
        invoke("set_user_volume", {
          username,
          gain: dbToGain(clamped),
        }).catch(console.error);
      }
    },
    [username, setUserVolume, localMutedUsers]
  );

  const handleReset = useCallback(() => {
    handleVolumeChange(DEFAULT_DB);
  }, [handleVolumeChange]);

  const handleToggleMute = useCallback(() => {
    if (!username) return;
    const willMute = !localMutedUsers.has(username);
    toggleLocalMute(username);
    if (willMute) {
      // Mute: set gain to 0
      invoke("set_user_volume", { username, gain: 0 }).catch(console.error);
    } else {
      // Unmute: restore previous volume
      const db = userVolumes[username] ?? DEFAULT_DB;
      invoke("set_user_volume", { username, gain: dbToGain(db) }).catch(console.error);
    }
  }, [username, localMutedUsers, toggleLocalMute, userVolumes]);

  // Close on outside click
  useEffect(() => {
    if (!username) return;
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        closeContextMenu();
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeContextMenu();
    };
    // Delay listener to avoid immediately closing from the triggering right-click
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", handleClick);
      document.addEventListener("keydown", handleKey);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [username, closeContextMenu]);

  if (!username || !anchor) return null;

  // Clamp position to stay within viewport
  const menuWidth = 220;
  const menuHeight = 140;
  const x = Math.min(anchor.x, window.innerWidth - menuWidth - 8);
  const y = Math.min(anchor.y, window.innerHeight - menuHeight - 8);

  // Map slider 0-100 range to dB: we use a linear mapping from MIN_DB to MAX_DB
  const sliderValue = ((currentDb - MIN_DB) / (MAX_DB - MIN_DB)) * 100;
  const handleSlider = (val: number) => {
    const db = MIN_DB + (val / 100) * (MAX_DB - MIN_DB);
    // Snap to 0 dB when close
    handleVolumeChange(Math.abs(db) < 0.8 ? 0 : Math.round(db * 10) / 10);
  };

  // Position of the 0 dB mark on the slider (percentage)
  const zeroDbPos = ((0 - MIN_DB) / (MAX_DB - MIN_DB)) * 100;

  return createPortal(
    <div
      ref={menuRef}
      className="fixed z-[100] w-[220px] rounded-xl border border-border bg-bg-secondary shadow-2xl animate-in fade-in zoom-in-95 duration-100"
      style={{ left: x, top: y }}
    >
      {/* Header */}
      <div className="flex items-center gap-2 border-b border-border px-3 py-2.5">
        <div
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-[10px] font-bold text-white"
          style={{ background: stringToGradient(username) }}
        >
          {username.charAt(0).toUpperCase()}
        </div>
        <span className="truncate text-[12px] font-bold text-text-bright">
          {username}
        </span>
      </div>

      {/* Mute toggle — hidden for own user */}
      {username !== currentUsername && (
        <button
          onClick={handleToggleMute}
          className="flex w-full items-center gap-2.5 px-3 py-2 text-[12px] font-medium transition-colors hover:bg-surface-hover"
        >
          <svg
            className={`h-4 w-4 shrink-0 ${isLocallyMuted ? "text-accent" : "text-text-muted"}`}
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M11 5L6 9H2v6h4l5 4V5z" />
            <line x1="23" y1="9" x2="17" y2="15" />
            <line x1="17" y1="9" x2="23" y2="15" />
          </svg>
          <span className={isLocallyMuted ? "text-accent" : "text-text-secondary"}>
            {isLocallyMuted ? "Muted" : "Mute"}
          </span>
        </button>
      )}

      {/* Volume control — hidden for own user */}
      {username !== currentUsername && (
        <div className="px-3 py-3">
          <div className="mb-2 flex items-center justify-between">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              User Volume
            </span>
            <button
              onClick={handleReset}
              className="text-[10px] font-semibold text-text-muted transition-colors hover:text-text-secondary"
              title="Reset to 0 dB"
            >
              Reset
            </button>
          </div>

          <div className="relative">
            <input
              type="range"
              min={0}
              max={100}
              value={sliderValue}
              onChange={(e) => handleSlider(Number(e.target.value))}
              className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-text-muted/20 accent-accent [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent [&::-webkit-slider-thumb]:shadow-md"
            />
            {/* 0 dB tick mark */}
            <div
              className="absolute top-[9px] h-1.5 w-px bg-text-muted/40"
              style={{ left: `${zeroDbPos}%` }}
            />
          </div>

          <div className="mt-1.5 flex items-center justify-between">
            <span className="text-[11px] font-semibold text-text-secondary">
              {formatDb(currentDb)}
            </span>
            <span className="text-[10px] text-text-muted">
              {dbToPercent(currentDb)}
            </span>
          </div>
        </div>
      )}
    </div>,
    document.body
  );
}
