import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient, stringToColor } from "../../utils/colors";
import MessageBubble, { shouldGroup } from "../chat/MessageBubble";

const ERROR_MESSAGES = [
  "This user is currently offline. Your message could not be delivered.",
  "This user only accepts direct messages from users in their friends list.",
];

export default function DmChatPanel() {
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const conversations = useDmStore((s) => s.conversations);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const localUsername = useAuthStore((s) => s.username);
  const dmFriendsPanelVisible = useUiStore((s) => s.dmFriendsPanelVisible);
  const toggleDmFriendsPanel = useUiStore((s) => s.toggleDmFriendsPanel);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const conversation = activeDmUser
    ? conversations[activeDmUser]
    : null;
  const messages = conversation?.messages ?? [];

  const friend = activeDmUser
    ? friends.find((f) => f.username === activeDmUser)
    : null;
  const isOnline =
    friend?.status === "online" ||
    (activeDmUser ? onlineUsers.includes(activeDmUser) : false);

  // Reset on conversation switch
  useEffect(() => {
    setSendError(null);
    setInput("");
  }, [activeDmUser]);

  // Auto-scroll on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    const id = requestAnimationFrame(() => {
      el.style.height = "0";
      el.style.height = Math.min(el.scrollHeight, 160) + "px";
    });
    return () => cancelAnimationFrame(id);
  }, [input]);

  // Auto-focus textarea when user starts typing anywhere
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key.length !== 1) return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      textareaRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleSend = async () => {
    if (!input.trim() || !activeDmUser) return;
    setSending(true);
    setSendError(null);
    try {
      await invoke("send_private_message", {
        recipient: activeDmUser,
        message: input.trim(),
      });
      setInput("");
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // Empty state — no active DM
  if (!activeDmUser) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-tertiary">
        <p className="text-sm text-text-muted">
          Select a conversation or start a new one
        </p>
      </div>
    );
  }

  // Map DmMessages to Message shape for MessageBubble compatibility
  const bubbleMessages = messages.map((m) => ({
    ...m,
    channelId: "",
  }));

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-tertiary">
      {/* DM header */}
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border px-4">
        <div className="relative">
          <div
            className="flex h-[26px] w-[26px] items-center justify-center rounded-lg text-[11px] font-bold text-white"
            style={{ background: stringToGradient(activeDmUser) }}
          >
            {activeDmUser.charAt(0).toUpperCase()}
          </div>
          <div
            className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-tertiary ${
              isOnline ? "bg-success" : "bg-text-muted"
            }`}
          />
        </div>
        <span className="text-[15px] font-bold text-text-bright">
          {activeDmUser}
        </span>
        <span
          className={`text-xs font-medium ${
            isOnline ? "text-success" : "text-text-muted"
          }`}
        >
          {isOnline ? "Online" : "Offline"}
        </span>
        <div className="flex-1" />
        <div className="flex gap-1">
          <button className="flex h-8 w-8 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" />
            </svg>
          </button>
          <button
            onClick={toggleDmFriendsPanel}
            className={`flex h-8 w-8 items-center justify-center rounded-md transition-colors ${
              dmFriendsPanelVisible
                ? "text-text-secondary bg-surface-hover"
                : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
            }`}
            title={dmFriendsPanelVisible ? "Hide friends" : "Show friends"}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />
            </svg>
          </button>
        </div>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-4 py-4">
        {messages.length === 0 ? (
          <div className="animate-[fadeUp_0.4s_ease_both]">
            <div className="border-b border-border pb-5 mb-5">
              <div
                className="mb-3 flex h-[60px] w-[60px] items-center justify-center rounded-xl text-[26px] font-bold text-white"
                style={{ background: stringToGradient(activeDmUser) }}
              >
                {activeDmUser.charAt(0).toUpperCase()}
              </div>
              <h1 className="mb-1.5 text-[26px] font-semibold tracking-tight text-text-bright">
                {activeDmUser}
              </h1>
              <p className="text-sm text-text-secondary leading-relaxed">
                This is the beginning of your conversation with{" "}
                <span
                  className="font-semibold"
                  style={{ color: stringToColor(activeDmUser) }}
                >
                  {activeDmUser}
                </span>
                .
              </p>
            </div>
          </div>
        ) : (
          bubbleMessages.map((msg, i) => {
            const isError =
              msg.sender === localUsername &&
              ERROR_MESSAGES.includes(msg.content);
            if (isError) {
              return (
                <div
                  key={`${msg.timestamp}-${msg.sender}-${i}`}
                  className="flex gap-3 px-2 py-1"
                >
                  <div className="w-[38px] shrink-0" />
                  <p className="text-[13px] italic text-error">
                    {msg.content}
                  </p>
                </div>
              );
            }
            return (
              <MessageBubble
                key={`${msg.timestamp}-${msg.sender}-${i}`}
                message={msg}
                grouped={shouldGroup(
                  i > 0 ? bubbleMessages[i - 1] : undefined,
                  msg
                )}
              />
            );
          })
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar */}
      <div className="px-4 pb-[18px]">
        <div className="flex items-center gap-2.5 rounded-xl border border-border bg-bg-secondary px-3.5 py-[11px] transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
          <textarea
            ref={textareaRef}
            rows={1}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={sending}
            placeholder={`Message @${activeDmUser}`}
            className="flex-1 resize-none bg-transparent text-sm leading-snug text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
            style={{ maxHeight: 160 }}
          />
        </div>
      </div>
    </div>
  );
}
