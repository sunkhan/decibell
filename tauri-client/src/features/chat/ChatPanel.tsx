import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";
import WelcomeState from "./WelcomeState";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";

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
  const hasMoreHistory = useChatStore((s) => s.hasMoreHistory);
  const historyLoading = useChatStore((s) => s.historyLoading);
  const historyFetched = useChatStore((s) => s.historyFetched);
  const activeView = useUiStore((s) => s.activeView);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);

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
    setPickerOpen(false);
    const stored = activeChannelId
      ? useDraftsStore.getState().channelDrafts[activeChannelId] ?? ""
      : "";
    editorRef.current?.setValue(stored);
    setInput(stored);
  }, [activeChannelId]);

  // First-open history fetch. Fires once per channel the user visits; server
  // caps the page size so this is bounded regardless of channel volume.
  useEffect(() => {
    if (!activeServerId || !activeChannelId) return;
    if (historyFetched[activeChannelId]) return;
    useChatStore.getState().markHistoryFetched(activeChannelId);
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: 0,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  }, [activeServerId, activeChannelId, historyFetched]);

  // Scroll-to-top pagination. When the user scrolls within ~100px of the top
  // and there's more history, fetch the next older page using the oldest id
  // currently loaded as the cursor.
  const handleScroll = () => {
    const el = messagesContainerRef.current;
    if (!el || !activeServerId || !activeChannelId) return;
    if (el.scrollTop > 100) return;
    if (!hasMoreHistory[activeChannelId]) return;
    if (historyLoading[activeChannelId]) return;
    const firstId = messages.find((m) => m.id !== 0)?.id ?? 0;
    if (firstId === 0) return;
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: firstId,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history (paginate) failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  };

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  // Auto-focus editor when user starts typing anywhere
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key.length !== 1) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (target?.isContentEditable) return;
      editorRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleSend = async () => {
    const value = editorRef.current?.getValue() ?? input;
    if (!value.trim() || !activeServerId || !activeChannelId) return;
    setSending(true);
    setSendError(null);
    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: value.trim(),
      });
      editorRef.current?.clear();
      setInput("");
      useDraftsStore.getState().clearChannelDraft(activeChannelId);
      setPickerOpen(false);
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  const handleInputChange = (value: string) => {
    setInput(value);
    if (activeChannelId) {
      useDraftsStore.getState().setChannelDraft(activeChannelId, value);
    }
  };

  const insertEmoji = (emoji: string) => {
    editorRef.current?.insertEmoji(emoji);
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
      <div
        ref={messagesContainerRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto px-4 py-4"
      >
        {activeChannelId && historyLoading[activeChannelId] && (
          <div className="flex justify-center py-2 text-[11px] text-text-muted">
            Loading older messages…
          </div>
        )}
        {activeChannelId &&
          !historyLoading[activeChannelId] &&
          hasMoreHistory[activeChannelId] === false &&
          messages.length > 0 && (
            <div className="flex justify-center py-2 text-[11px] text-text-muted">
              Start of #{channelName ?? "channel"}
            </div>
          )}
        {messages.length === 0 && !historyLoading[activeChannelId ?? ""] ? (
          <WelcomeState channelName={channelName ?? "channel"} />
        ) : (
          messages.map((msg, i) => (
            <MessageBubble
              key={msg.id !== 0 ? msg.id : `${msg.timestamp}-${msg.sender}-${i}`}
              message={msg}
              grouped={shouldGroup(messages[i - 1], msg)}
              serverId={activeServerId}
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
          <RichInput
            ref={editorRef}
            onChange={handleInputChange}
            onEnter={handleSend}
            disabled={sending}
            placeholder={`Message #${channelName ?? "channel"}`}
            className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            maxHeight={160}
          />
          <div className="flex shrink-0 self-end gap-1">
            <div className="relative">
              <button
                ref={emojiTriggerRef}
                onClick={() => setPickerOpen((v) => !v)}
                className={`flex h-7 w-7 cursor-pointer items-center justify-center rounded-md transition-colors ${
                  pickerOpen
                    ? "bg-surface-hover text-text-secondary"
                    : "text-text-muted hover:text-text-secondary"
                }`}
                title="Emoji"
              >
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <circle cx="12" cy="12" r="10" /><path d="M8 14s1.5 2 4 2 4-2 4-2" /><line x1="9" y1="9" x2="9.01" y2="9" /><line x1="15" y1="9" x2="15.01" y2="9" />
                </svg>
              </button>
              {pickerOpen && (
                <EmojiPicker
                  onSelect={(emoji) => insertEmoji(emoji)}
                  onClose={() => setPickerOpen(false)}
                  triggerRef={emojiTriggerRef}
                />
              )}
            </div>
            <button
              onClick={handleSend}
              disabled={sending || !input.trim()}
              className="flex h-[34px] w-[34px] shrink-0 items-center justify-center rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
