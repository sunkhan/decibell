import { useState, useEffect, useRef } from "react";
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
  const closePopup = useUiStore((s) => s.closeProfilePopup);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const currentUsername = useAuthStore((s) => s.username);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const popupRef = useRef<HTMLDivElement>(null);

  // Reset input when popup opens for a different user
  useEffect(() => {
    setInput("");
    setSending(false);
  }, [username]);

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

  // Clamp popup position to viewport
  const popupWidth = 320;
  const popupHeight = 260;
  const x = Math.min(anchor.x, window.innerWidth - popupWidth - 16);
  const y = Math.min(anchor.y, window.innerHeight - popupHeight - 16);

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

  return createPortal(
    <div className="fixed inset-0 z-[100]">
      <div
        ref={popupRef}
        className="absolute animate-[fadeUp_0.2s_ease_both] overflow-hidden rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        style={{ left: x, top: y, width: popupWidth }}
      >
        {/* Banner */}
        <div
          className="h-[70px]"
          style={{ background: stringToGradient(username) }}
        />

        {/* Avatar */}
        <div className="px-5">
          <div className="relative -mt-9">
            <div
              className="flex h-[72px] w-[72px] items-center justify-center rounded-xl border-4 border-bg-secondary text-[28px] font-semibold text-white"
              style={{ background: stringToGradient(username) }}
            >
              {username.charAt(0).toUpperCase()}
            </div>
            <div
              className={`absolute -bottom-0.5 -right-0.5 h-4 w-4 rounded-full border-[3px] border-bg-secondary ${
                isOnline ? "bg-success" : "bg-text-muted"
              }`}
            />
          </div>
        </div>

        {/* Info */}
        <div className="px-5 pt-3">
          <div className="text-lg font-semibold text-text-bright">
            {username}
          </div>
          <div
            className={`mt-0.5 text-xs font-semibold ${
              isOnline ? "text-success" : "text-text-muted"
            }`}
          >
            {isOnline ? "Online" : "Offline"}
          </div>
        </div>

        {/* Message input — hidden for own user */}
        {username !== currentUsername && (
          <>
            <div className="mx-5 my-3.5 h-px bg-border" />
            <div className="px-3.5 pb-3.5">
              <div className="flex items-center rounded-xl border border-border bg-bg-tertiary px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
                <input
                  type="text"
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  onKeyDown={handleKeyDown}
                  disabled={sending}
                  placeholder={`Message @${username}`}
                  className="flex-1 bg-transparent text-sm text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
                  autoFocus
                />
              </div>
            </div>
          </>
        )}
      </div>
    </div>,
    document.body
  );
}
