import { useState, useRef, useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { toast } from "../../stores/toastStore";
import { stringToGradient, stringToColor } from "../../utils/colors";
import MessageBubble, { shouldGroup } from "../chat/MessageBubble";
import EmojiPicker from "../chat/EmojiPicker";
import ErrorCard from "../../components/ErrorCard";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import DeleteMessageConfirmModal from "../../components/DeleteMessageConfirmModal";
import type { DmMessage, Message } from "../../types";

// Canonical reject strings the central server echoes back as a DM
// from us-to-us. Pattern-matched here so we can render them as a
// distinct error banner instead of a normal message bubble.
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
  const activeModal = useUiStore((s) => s.activeModal);
  const openModal = useUiStore((s) => s.openModal);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pendingDeleteTarget, setPendingDeleteTarget] =
    useState<DmMessage | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);
  // Per-peer record of the most recently observed messages.length.
  // Used to skip the auto-scroll when length *decreases* (a delete or
  // an optimistic remove). Switching conversations still scrolls to
  // bottom because the new peer's entry starts at 0.
  const prevMessagesLenRef = useRef<Record<string, number>>({});

  // Fire the delete flow for a DM message. Optimistic: snapshot
  // into pendingDmDeletions, remove from the view, fire the native
  // command, and start a 5-second watchdog. useDmEvents handles
  // success/failure acks.
  const handleDeleteDmMessage = (message: DmMessage) => {
    if (!activeDmUser || typeof message.id !== "number") return;
    const peer = activeDmUser;
    const messageId = message.id;

    useDmStore.getState().snapshotAndRemoveDm(peer, messageId);

    invoke("delete_dm_message", { peer, messageId }).catch((err) => {
      console.error("delete_dm_message:", err);
      useDmStore.getState().restorePendingDmDeletion(peer, messageId);
      toast.error("Failed to delete message", "Please try again.");
    });

    window.setTimeout(() => {
      const stillPending = useDmStore
        .getState()
        .pendingDmDeletions[peer]?.has(messageId);
      if (stillPending) {
        useDmStore.getState().restorePendingDmDeletion(peer, messageId);
        toast.error(
          "Delete timed out",
          "Couldn't reach the server. Please try again.",
        );
      }
    }, 5000);
  };

  const requestDeleteDmMessage = (
    message: Message,
    options?: { skipConfirm?: boolean },
  ) => {
    if (typeof message.id !== "number" || message.id <= 0) return;
    // Message and DmMessage are structurally compatible for what we
    // need (id, sender, content, timestamp). Cast to DmMessage for
    // the local state — MessageBubble passes a Message at the prop
    // boundary; underneath it's the same object.
    if (options?.skipConfirm) {
      // Shift+click: power-user path. Delete immediately, no modal.
      handleDeleteDmMessage(message as DmMessage);
      return;
    }
    setPendingDeleteTarget(message as DmMessage);
    openModal("delete-message-confirm");
  };

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

  // Restore draft on conversation switch.
  useEffect(() => {
    setSendError(null);
    setPickerOpen(false);
    const stored = activeDmUser
      ? useDraftsStore.getState().dmDrafts[activeDmUser] ?? ""
      : "";
    editorRef.current?.setValue(stored);
    setInput(stored);
  }, [activeDmUser]);

  // On switching to a peer, pull the latest page of history IF we
  // haven't already loaded server history for this conversation in
  // this session. Live in-memory messages aren't enough to know we've
  // "seen" the full history; the server's view is authoritative.
  useEffect(() => {
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (conv?.historyLoaded) return;
    invoke("request_dm_history", {
      peer: activeDmUser,
      beforeId: 0,
      limit: 50,
    }).catch(console.error);
  }, [activeDmUser]);

  // Scroll-up paginator. Fires when the message list scrolls near
  // the top and the server says there are older messages available.
  // Single-flight via the ref so rapid scroll doesn't fire parallel
  // pages.
  const loadMoreInFlightRef = useRef(false);
  const onScrollLoadMore = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (el.scrollTop > 80) return;
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (!conv?.hasMoreHistory) return;
    if (loadMoreInFlightRef.current) return;
    loadMoreInFlightRef.current = true;
    const oldest = conv.messages.find(
      (m): m is typeof m & { id: number } => typeof m.id === "number" && m.id > 0,
    );
    const beforeId = oldest?.id ?? 0;
    invoke("request_dm_history", {
      peer: activeDmUser,
      beforeId,
      limit: 50,
    })
      .catch(console.error)
      .finally(() => {
        loadMoreInFlightRef.current = false;
      });
  };

  useEffect(() => {
    if (!activeDmUser) return;
    const prev = prevMessagesLenRef.current[activeDmUser] ?? 0;
    prevMessagesLenRef.current[activeDmUser] = messages.length;
    // Only scroll when the list grew — new send/receive, history page
    // arriving, or first open of this conversation. Skips the case
    // where a delete (or any other shrink) reduces the count.
    if (messages.length > prev) {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [messages.length, activeDmUser]);

  // Debounced mark-read. Fires whenever the local user is viewing
  // the panel for a peer and there are unread messages with a real
  // id. Optimistically zeroes the unread count locally; server call
  // is fire-and-forget.
  const conversationForActive = activeDmUser
    ? conversations[activeDmUser]
    : undefined;
  const messagesLenForMarkRead = conversationForActive?.messages.length ?? 0;
  useEffect(() => {
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (!conv) return;
    let latestId = 0;
    for (const m of conv.messages) {
      if (typeof m.id === "number" && m.id > latestId) latestId = m.id;
    }
    if (latestId === 0 || latestId <= conv.lastReadId) return;
    // Optimistic local clear; server sync follows after a small
    // coalesce window so a burst of new messages results in a
    // single mark-read RPC. Capture peer + upToId into closure consts
    // so the fire isn't tripped by later activeDmUser/latestId mutation.
    useDmStore.getState().markRead(activeDmUser, latestId);
    const peer = activeDmUser;
    const upToId = latestId;
    // Intentionally NO cleanup return. React StrictMode in dev would
    // otherwise cancel this timeout immediately, and the second mount
    // would early-return on the now-bumped lastReadId — so the RPC
    // would never fire on the view path. Semantically we also want
    // the RPC to fire even if the user navigates away within 250ms;
    // they read those messages, mark them read. Server's GREATEST
    // upsert dedupes any extra fires in a burst.
    window.setTimeout(() => {
      invoke("mark_dm_read", { peer, upToId }).catch(console.error);
    }, 250);
  }, [activeDmUser, messagesLenForMarkRead]);

  // Auto-focus the editor when the user starts typing anywhere — same
  // ergonomics as the channel chat panel.
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

  if (!activeDmUser) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid">
        <p className="text-sm text-text-muted">
          Select a conversation or start a new one
        </p>
      </div>
    );
  }

  // Map DmMessages to Message shape for MessageBubble compatibility.
  // Preserve the real server-assigned id when present (persistent-DMs)
  // — the delete flow keys on it. Legacy / synthetic preview entries
  // (pre-persistence DMs) fall back to 0 and the trash icon won't
  // appear for those, which is correct (nothing to delete).
  const bubbleMessages = messages.map((m) => ({
    ...m,
    id: typeof m.id === "number" ? m.id : 0,
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
      <div
        className="flex-1 overflow-y-auto pr-4 py-4"
        onScroll={onScrollLoadMore}
      >
        {messages.length === 0 ? (
          <div className="animate-[fadeUp_0.4s_ease_both] pl-4">
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
                  className="pl-4 pr-2 py-1.5"
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
                // Align avatar's left edge with the input bar card's
                // left edge: outer wrapper `px-3` = 12px from chat
                // panel's left. The card's rounded border starts there.
                paddingLeft={12}
                canDelete={
                  typeof msg.id === "number" &&
                  msg.id > 0 &&
                  msg.sender === localUsername
                }
                onDelete={requestDeleteDmMessage}
              />
            );
          })
        )}
        <div ref={messagesEndRef} />
      </div>

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
          {/* Emoji + send buttons grouped in an inner flex so the
              gap between them is gap-1 (4px) — matches the server-
              channel ChatPanel pattern. Otherwise they'd be siblings
              of the outer gap-2.5 (10px) parent and visually sit too
              far apart. self-end is on the wrapper, not each button,
              so they slide together when the textarea grows
              multi-line. */}
          <div className="flex shrink-0 self-end gap-1">
            <div className="relative">
              <button
                ref={emojiTriggerRef}
                onClick={() => setPickerOpen((v) => !v)}
                className={`flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-md transition-colors ${
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
              className="flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
              </svg>
            </button>
          </div>
        </div>
      </div>

      {activeModal === "delete-message-confirm" && pendingDeleteTarget && (
        <DeleteMessageConfirmModal
          onConfirm={() => {
            handleDeleteDmMessage(pendingDeleteTarget);
            setPendingDeleteTarget(null);
          }}
        />
      )}
    </div>
  );
}
