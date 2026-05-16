import { useEffect, useRef, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToGradient } from "../../utils/colors";
import { saveSettings } from "../settings/saveSettings";

const MIN_DB = -40;
const MAX_DB = 15;
const DEFAULT_DB = 0;

function dbToGain(db: number): number {
  if (db <= MIN_DB) return 0;
  return Math.pow(10, db / 20);
}

function formatDb(db: number): string {
  if (db <= MIN_DB) return "Muted";
  if (db === 0) return "0 dB";
  return `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`;
}

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

  const currentDb = username ? userVolumes[username] ?? DEFAULT_DB : DEFAULT_DB;

  const handleVolumeChange = useCallback(
    (db: number) => {
      if (!username) return;
      const clamped = Math.max(MIN_DB, Math.min(MAX_DB, db));
      setUserVolume(username, clamped);
      if (!localMutedUsers.has(username)) {
        invoke("set_user_volume", {
          username,
          gain: dbToGain(clamped),
        }).catch(console.error);
      }
      saveSettings();
    },
    [username, setUserVolume, localMutedUsers],
  );

  const handleReset = useCallback(() => {
    handleVolumeChange(DEFAULT_DB);
  }, [handleVolumeChange]);

  const handleToggleMute = useCallback(() => {
    if (!username) return;
    const willMute = !localMutedUsers.has(username);
    toggleLocalMute(username);
    if (willMute) {
      invoke("set_user_volume", { username, gain: 0 }).catch(console.error);
    } else {
      const db = userVolumes[username] ?? DEFAULT_DB;
      invoke("set_user_volume", { username, gain: dbToGain(db) }).catch(console.error);
    }
    saveSettings();
  }, [username, localMutedUsers, toggleLocalMute, userVolumes]);

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

  const menuWidth = 220;
  const menuHeight = 160;
  const x = Math.min(anchor.x, window.innerWidth - menuWidth - 8);
  const y = Math.min(anchor.y, window.innerHeight - menuHeight - 8);

  const sliderValue = ((currentDb - MIN_DB) / (MAX_DB - MIN_DB)) * 100;
  const handleSlider = (val: number) => {
    const db = MIN_DB + (val / 100) * (MAX_DB - MIN_DB);
    handleVolumeChange(Math.abs(db) < 0.8 ? 0 : Math.round(db * 10) / 10);
  };
  const zeroDbPos = ((0 - MIN_DB) / (MAX_DB - MIN_DB)) * 100;

  return createPortal(
    <div
      ref={menuRef}
      className="fixed z-[100] w-[220px] animate-[dropIn_0.15s_ease] overflow-hidden rounded-xl border border-border bg-bg-light shadow-[0_8px_32px_rgba(0,0,0,0.45),0_0_0_1px_rgba(255,255,255,0.02)]"
      style={{ left: x, top: y }}
    >
      {/* User header */}
      <div className="flex items-center gap-2.5 border-b border-border-divider px-3.5 py-3">
        <div
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-[13px] font-semibold text-white"
          style={{ background: stringToGradient(username) }}
        >
          {username.charAt(0).toUpperCase()}
        </div>
        <span className="truncate font-display text-[14px] font-medium text-text-primary">
          {username}
        </span>
      </div>

      {username !== currentUsername && (
        <>
          {/* Mute button */}
          <div className="px-[5px] py-1">
            <button
              onClick={handleToggleMute}
              className={`flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-[13px] font-medium transition-colors ${
                isLocallyMuted
                  ? "bg-error/10 text-error"
                  : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              <svg
                className={`h-4 w-4 shrink-0 ${
                  isLocallyMuted ? "text-error" : "text-text-muted"
                }`}
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                {isLocallyMuted ? (
                  <>
                    <path d="M11 5L6 9H2v6h4l5 4V5z" />
                    <path d="M19.07 4.93a10 10 0 010 14.14" />
                    <path d="M15.54 8.46a5 5 0 010 7.07" />
                  </>
                ) : (
                  <>
                    <path d="M11 5L6 9H2v6h4l5 4V5z" />
                    <line x1="23" y1="9" x2="17" y2="15" />
                    <line x1="17" y1="9" x2="23" y2="15" />
                  </>
                )}
              </svg>
              {isLocallyMuted ? "Unmute" : "Mute"}
            </button>
          </div>

          {/* Divider */}
          <div className="mx-2.5 h-px bg-border-divider" />

          {/* Volume section */}
          <div className="px-3.5 py-3">
            <div className="mb-2.5 flex items-center justify-between">
              <span className="text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                User volume
              </span>
              <button
                onClick={handleReset}
                className="text-[11px] font-medium text-accent-bright transition-colors hover:text-accent"
                title="Reset to 0 dB"
              >
                Reset
              </button>
            </div>

            {/* Slider */}
            <div className="relative mb-2">
              <input
                type="range"
                min={0}
                max={100}
                value={sliderValue}
                onChange={(e) => handleSlider(Number(e.target.value))}
                className="h-[4px] w-full cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[14px] [&::-webkit-slider-thumb]:w-[14px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
              />
              {/* 0 dB tick mark */}
              <div
                className="absolute top-[7px] h-[4px] w-px bg-text-muted/40"
                style={{ left: `${zeroDbPos}%` }}
              />
            </div>

            {/* Labels */}
            <div className="flex items-center justify-between">
              <span className="text-[11px] font-medium tabular-nums text-text-primary">
                {formatDb(currentDb)}
              </span>
              <span className="text-[11px] tabular-nums text-text-muted">
                {dbToPercent(currentDb)}
              </span>
            </div>
          </div>
        </>
      )}
    </div>,
    document.body,
  );
}
