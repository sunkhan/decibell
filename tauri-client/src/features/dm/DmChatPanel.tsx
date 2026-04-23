import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { stringToGradient, stringToColor } from "../../utils/colors";
import MessageBubble, { shouldGroup } from "../chat/MessageBubble";
import EmojiPicker from "../chat/EmojiPicker";
import ErrorCard from "../../components/ErrorCard";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";

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
  const [pickerOpen, setPickerOpen] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);

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

  // Restore draft on conversation switch
  useEffect(() => {
    setSendError(null);
    setPickerOpen(false);
    const stored = activeDmUser
      ? useDraftsStore.getState().dmDrafts[activeDmUser] ?? ""
      : "";
    editorRef.current?.setValue(stored);
    setInput(stored);
  }, [activeDmUser]);

  // Auto-scroll on new messages
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
    if (!value.trim() || !activeDmUser) return;
    setSending(true);
    setSendError(null);
    try {
      await invoke("send_private_message", {
        recipient: activeDmUser,
        message: value.trim(),
      });
      editorRef.current?.clear();
      setInput("");
      useDraftsStore.getState().clearDmDraft(activeDmUser);
      setPickerOpen(false);
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  const handleInputChange = (value: string) => {
    setInput(value);
    if (activeDmUser) {
      useDraftsStore.getState().setDmDraft(activeDmUser, value);
    }
  };

  const insertEmoji = (emoji: string) => {
    editorRef.current?.insertEmoji(emoji);
  };

  // Empty state — no active DM
  if (!activeDmUser) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid">
        <p className="text-sm text-text-muted">
          Select a conversation or start a new one
        </p>
      </div>
    );
  }

  // Map DmMessages to Message shape for MessageBubble compatibility. DMs
  // aren't persisted server-side, so id is always 0 and attachments are empty.
  const bubbleMessages = messages.map((m) => ({
    ...m,
    id: 0,
    channelId: "",
    attachments: [],
  }));

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
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
        <span className="font-display text-[15px] font-semibold text-text-bright">
          {activeDmUser}
        </span>
        {isOnline ? (
          <div className="flex items-center gap-[5px] rounded bg-success/15 px-2 py-0.5 font-channel text-[11px] font-medium text-success">
            <div className="h-1.5 w-1.5 rounded-full bg-success" />
            Online
          </div>
        ) : (
          <div className="flex items-center gap-[5px] rounded bg-text-muted/15 px-2 py-0.5 font-channel text-[11px] font-medium text-text-muted">
            <div className="h-1.5 w-1.5 rounded-full bg-text-muted" />
            Offline
          </div>
        )}
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
                  className="px-2 py-1.5"
                >
                  <ErrorCard>
                    {msg.content === ERROR_MESSAGES[0] ? (
                      <>
                        <span className="font-medium text-warning">User is offline.</span>{" "}
                        Your message could not be delivered. It will be sent when {activeDmUser} comes back online.
                      </>
                    ) : (
                      <>
                        <span className="font-medium text-warning">Can't reach this user.</span>{" "}
                        They only accept direct messages from users in their friends list.
                      </>
                    )}
                  </ErrorCard>
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
      <div className="px-3 pb-2">
        <div className="flex min-h-[54px] items-center gap-2.5 rounded-xl border border-border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
          <RichInput
            ref={editorRef}
            onChange={handleInputChange}
            onEnter={handleSend}
            disabled={sending}
            placeholder={`Message @${activeDmUser}`}
            className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            maxHeight={160}
          />
          <div className="relative self-end">
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
            className="flex h-[34px] w-[34px] shrink-0 items-center justify-center self-end rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
          >
            <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
              <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
