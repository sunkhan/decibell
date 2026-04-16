import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";
import WelcomeState from "./WelcomeState";

export function ChatHeader() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const membersPanelVisible = useUiStore((s) => s.membersPanelVisible);
  const toggleMembersPanel = useUiStore((s) => s.toggleMembersPanel);

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  if (!activeChannelId) return null;

  return (
    <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border bg-bg-mid px-4">
      <span className="font-channel text-[16px] font-medium text-text-muted">#</span>
      <span className="font-display text-[15px] font-semibold text-text-bright">
        {channelName ?? activeChannelId}
      </span>
      <div className="h-5 w-px bg-border-divider" />
      <span className="flex-1 text-[13px] text-text-muted">
        The main hangout — say whatever
      </span>
      <div className="flex gap-1">
        <button className="flex h-8 w-8 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
        </button>
        <button
          onClick={toggleMembersPanel}
          className={`flex h-8 w-8 items-center justify-center rounded-md transition-colors ${
            membersPanelVisible
              ? "text-text-secondary bg-surface-hover"
              : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
          }`}
          title={membersPanelVisible ? "Hide members" : "Show members"}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />
          </svg>
        </button>
      </div>
    </div>
  );
}

export default function ChatPanel({ hideHeader = false }: { hideHeader?: boolean }) {
  useChatEvents();

  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const messagesByChannel = useChatStore((s) => s.messagesByChannel);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const activeView = useUiStore((s) => s.activeView);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const messages = activeChannelId
    ? messagesByChannel[activeChannelId] ?? []
    : [];

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  useEffect(() => {
    setSendError(null);
    setInput("");
  }, [activeChannelId]);

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
      if (e.key.length !== 1) return; // ignore non-printable keys
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return; // already in an input
      textareaRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleSend = async () => {
    if (!input.trim() || !activeServerId || !activeChannelId) return;
    setSending(true);
    setSendError(null);
    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
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

  // Empty state
  if (activeView === "home" || !activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid">
        <p className="text-sm text-text-muted">
          Select a channel to start chatting
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      {/* Channel header */}
      {!hideHeader && <ChatHeader />}

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-4 py-4">
        {messages.length === 0 ? (
          <WelcomeState channelName={channelName ?? "channel"} />
        ) : (
          messages.map((msg, i) => (
            <MessageBubble
              key={`${msg.timestamp}-${msg.sender}-${i}`}
              message={msg}
              grouped={shouldGroup(messages[i - 1], msg)}
            />
          ))
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar */}
      <div className="px-3 pb-2">
        <div className="flex min-h-[54px] items-center gap-2.5 rounded-xl border border-border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
          <button className="flex h-7 w-7 shrink-0 self-end items-center justify-center rounded-full bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="12" cy="12" r="10" /><line x1="12" y1="8" x2="12" y2="16" /><line x1="8" y1="12" x2="16" y2="12" />
            </svg>
          </button>
          <textarea
            ref={textareaRef}
            rows={1}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={sending}
            placeholder={`Message #${channelName ?? "channel"}`}
            className="flex-1 resize-none bg-transparent text-sm leading-snug text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
            style={{ maxHeight: 160 }}
          />
          <div className="flex shrink-0 self-end gap-1">
            <button className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:text-text-secondary">
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <circle cx="12" cy="12" r="10" /><path d="M8 14s1.5 2 4 2 4-2 4-2" /><line x1="9" y1="9" x2="9.01" y2="9" /><line x1="15" y1="9" x2="15.01" y2="9" />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
