import { useState, useEffect, useLayoutEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient } from "../../utils/colors";
import { UserAvatar } from "../../components/UserAvatar";

// Anchored profile popup. Triggered from anywhere a username is shown
// (members list, message bubble click, etc.) by calling
// useUiStore.openProfilePopup(username, {x, y}, serverId?). serverId
// is optional — present in server-context invocations so the popup can
// render role chips; null in pure DM/friends contexts.
//
// The popup anchors at (anchor.x, anchor.y) but clamps its final
// y-position post-measurement so the full card stays inside the
// viewport even when the trigger is near the bottom of the screen.
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
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setInput("");
    setSending(false);
  }, [username]);

  // Measure rendered height and reposition so the full card stays in
  // the viewport. Reset clampedY when inputs change so the next mount
  // re-measures cleanly (otherwise the popup briefly renders at the
  // previous instance's position).
  useLayoutEffect(() => {
    if (!username || !anchor || !popupRef.current) {
      setClampedY(null);
      return;
    }
    const height = popupRef.current.getBoundingClientRect().height;
    const next = Math.max(16, Math.min(anchor.y, window.innerHeight - height - 16));
    setClampedY(next);
  }, [username, anchor, serverId]);

  // Focus the quick-DM input as soon as the popup measures and
  // becomes interactive (clampedY transitions from null to a number).
  useEffect(() => {
    if (clampedY === null) return;
    inputRef.current?.focus();
  }, [clampedY]);

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
          // Keep invisible until the post-measure clamp runs so the
          // popup doesn't flash at the un-clamped y on first frame.
          // We use opacity rather than `visibility: hidden` because
          // visibility:hidden on a parent makes the whole subtree
          // *unfocusable* — the quick-DM <input> below couldn't take
          // focus until the user clicked it manually.
          opacity: clampedY === null ? 0 : 1,
          pointerEvents: clampedY === null ? "none" : undefined,
          animation: clampedY === null ? undefined : "fadeUp 0.25s ease both",
        }}
      >
        <div className="h-[60px]" style={{ background: gradient }} />

        <div className="relative h-7 mx-4">
          <div className="absolute -top-9 left-0 rounded-2xl border-[4px] border-bg-light">
            <UserAvatar username={username} size={64} className="!rounded-[10px]" />
            <div
              className={`absolute -bottom-px -right-px h-[14px] w-[14px] rounded-full border-[3px] border-bg-light ${
                isOnline ? "bg-success" : "bg-text-muted"
              }`}
            />
          </div>
        </div>

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

        {/* Roles row — server context only, hard-coded to "Member" until
            server-side role data lands. */}
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

        {/* Quick-DM input — hidden when looking at our own profile. */}
        {username !== currentUsername && (
          <>
            <div className="mx-4 my-3 h-px bg-border-divider" />
            <div className="px-4 pb-4">
              <input
                ref={inputRef}
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                disabled={sending}
                placeholder={`Message @${username}`}
                className="w-full rounded-[10px] border border-border bg-bg-mid px-3 py-[9px] text-[12px] text-text-primary outline-none transition-all placeholder:text-text-faint focus:border-accent focus:shadow-[0_0_0_2px_var(--color-accent-soft)] disabled:opacity-50"
              />
            </div>
          </>
        )}
      </div>
    </div>,
    document.body
  );
}
