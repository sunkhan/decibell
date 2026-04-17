import { useState, useEffect, useLayoutEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient } from "../../utils/colors";

export default function UserProfilePopup() {
  const username = useUiStore((s) => s.profilePopupUser);
  const anchor = useUiStore((s) => s.profilePopupAnchor);
  const serverId = useUiStore((s) => s.profilePopupServerId);
  const closePopup = useUiStore((s) => s.closeProfilePopup);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const currentUsername = useAuthStore((s) => s.username);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [clampedY, setClampedY] = useState<number | null>(null);
  const popupRef = useRef<HTMLDivElement>(null);

  // Reset input when popup opens for a different user
  useEffect(() => {
    setInput("");
    setSending(false);
  }, [username]);

  // Measure actual rendered popup height and reposition so the full card stays inside the viewport.
  useLayoutEffect(() => {
    if (!username || !anchor || !popupRef.current) return;
    const height = popupRef.current.getBoundingClientRect().height;
    const next = Math.max(16, Math.min(anchor.y, window.innerHeight - height - 16));
    setClampedY(next);
  }, [username, anchor, serverId]);

  // Close on outside click
  useEffect(() => {
    if (!username) return;
    const handler = (e: MouseEvent) => {
      if (popupRef.current && !popupRef.current.contains(e.target as Node)) {
        closePopup();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [username, closePopup]);

  // Close on Escape
  useEffect(() => {
    if (!username) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") closePopup();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [username, closePopup]);

  if (!username || !anchor) return null;

  const friend = friends.find((f) => f.username === username);
  const isOnline =
    friend?.status === "online" || onlineUsers.includes(username);

  // Clamp popup position to viewport. y is finalized after measuring real content height.
  const popupWidth = 300;
  const x = Math.min(anchor.x, window.innerWidth - popupWidth - 16);
  const y = clampedY ?? anchor.y;

  const handleSend = async () => {
    if (!input.trim() || sending) return;
    setSending(true);
    try {
      await invoke("send_private_message", {
        recipient: username,
        message: input.trim(),
      });
      setInput("");
      closePopup();
      setActiveDmUser(username);
      setActiveView("dm");
    } catch (err) {
      console.error("DM send failed:", err);
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const gradient = stringToGradient(username);
  const initial = username.charAt(0).toUpperCase();

  return createPortal(
    <div className="fixed inset-0 z-[9999] isolate">
      <div
        ref={popupRef}
        className="absolute overflow-hidden rounded-[14px] border border-border bg-bg-light shadow-[0_12px_48px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.02)]"
        style={{
          left: x,
          top: y,
          width: popupWidth,
          maxHeight: `calc(100vh - 32px)`,
          transform: "translateZ(0)",
          visibility: clampedY === null ? "hidden" : "visible",
          animation: clampedY === null ? undefined : "fadeUp 0.25s ease both",
        }}
      >
        {/* Banner */}
        <div
          className="h-[60px]"
          style={{ background: gradient }}
        />

        {/* Avatar — overlaps banner */}
        <div className="relative h-7 mx-4">
          <div
            className="absolute -top-9 left-0 flex h-16 w-16 items-center justify-center rounded-2xl border-[4px] border-bg-light text-[22px] font-bold text-white"
            style={{ background: gradient }}
          >
            {initial}
            {/* Status dot */}
            <div
              className={`absolute -bottom-px -right-px h-[14px] w-[14px] rounded-full border-[3px] border-bg-light ${
                isOnline ? "bg-success" : "bg-text-muted"
              }`}
            />
          </div>
        </div>

        {/* Info */}
        <div className="px-4 pb-1">
          <div className="font-display text-[16px] font-semibold text-text-primary">
            {username}
          </div>
          <div className="mt-1.5">
            {isOnline ? (
              <span className="inline-flex items-center gap-[5px] rounded bg-success/15 px-2 py-0.5 text-[11px] font-medium text-success">
                <span className="h-1.5 w-1.5 rounded-full bg-success" />
                Online
              </span>
            ) : (
              <span className="inline-flex items-center gap-[5px] rounded bg-text-muted/15 px-2 py-0.5 text-[11px] font-medium text-text-muted">
                <span className="h-1.5 w-1.5 rounded-full bg-text-muted" />
                Offline
              </span>
            )}
          </div>
        </div>

        {/* Divider + Roles — only in server context */}
        {serverId && (
          <>
            <div className="mx-4 my-3 h-px bg-border-divider" />
            <div className={`px-4 ${username === currentUsername ? "pb-4" : ""}`}>
              <div className="mb-2 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                Roles
              </div>
              <div className="flex flex-wrap gap-1.5">
                <span className="inline-flex items-center gap-[5px] rounded border border-border-divider bg-bg-lighter px-2 py-[3px] text-[11px] font-medium text-text-secondary">
                  <span className="h-2 w-2 rounded-full bg-accent-bright" />
                  Member
                </span>
              </div>
            </div>
          </>
        )}

        {/* Message input — hidden for own user */}
        {username !== currentUsername && (
          <>
            <div className="mx-4 my-3 h-px bg-border-divider" />
            <div className="px-4 pb-4">
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                disabled={sending}
                placeholder={`Message @${username}`}
                className="w-full rounded-[10px] border border-border bg-bg-mid px-3 py-[9px] text-[12px] text-text-primary outline-none transition-all placeholder:text-text-faint focus:border-accent focus:shadow-[0_0_0_2px_var(--color-accent-soft)] disabled:opacity-50"
                autoFocus
              />
            </div>
          </>
        )}
      </div>
    </div>,
    document.body
  );
}
