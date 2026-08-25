import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { Virtuoso } from "react-virtuoso";
import { useVirtuosoPrepend } from "../chat/useVirtuosoPrepend";
import { invoke } from "../../lib/ipc";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { toast } from "../../stores/toastStore";
import { stringToColor } from "../../utils/colors";
import { UserAvatar } from "../../components/UserAvatar";
import MessageBubble, { shouldGroup } from "../chat/MessageBubble";
import { useTypeToFocusComposer } from "../chat/useTypeToFocusComposer";
import MessagePreview from "../chat/MessagePreview";
import RichComposer from "../chat/RichComposer";
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

// Keep in sync with ChatPanel's HISTORY_EAGER_THRESHOLD — same
// rationale, documented there.
const HISTORY_EAGER_THRESHOLD = 25;

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
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);
  // Per-peer scroll state. Saved on conversation switch via the
  // cleanup of the activeDmUser effect; restored on Virtuoso mount
  // through initialTopMostItemIndex. atBottom === true means the
  // user was caught up; we land them at the latest message so any
  // newer ones that arrived while away are visible.
  const savedPositionsRef = useRef<
    Record<string, { topIndex: number; atBottom: boolean }>
  >({});
  // Live cursors written by Virtuoso's rangeChanged / atBottomStateChange.
  // Persisted into savedPositionsRef on conversation switch.
  const topIndexRef = useRef<number>(0);
  const atBottomRef = useRef<boolean>(true);
  // Single-flight guard for the scroll-up paginator so rapid scroll
  // doesn't fire parallel page loads.
  const loadMoreInFlightRef = useRef(false);
  // Boundary dedup for the eager pagination trigger — the history
  // response arrives via event, not the invoke promise, so the
  // in-flight guard alone can't stop a re-request of the same page.
  const lastRequestedBeforeIdRef = useRef(0);

  // Fire the delete flow for a DM message. Optimistic: snapshot
  // into pendingDmDeletions, remove from the view, fire the native
  // command, and start a 5-second watchdog. useDmEvents handles
  // success/failure acks.
  // useCallback for the same reason as ChatPanel's delete pair: the
  // handler is a MessageBubble prop, and a fresh closure per render
  // defeats memo(MessageBubble) — every keystroke re-rendered every
  // visible bubble. Peer is read from the store at call time so the
  // callback identity doesn't churn on conversation switches either.
  const handleDeleteDmMessage = useCallback((message: DmMessage) => {
    const peer = useDmStore.getState().activeDmUser;
    if (!peer || typeof message.id !== "number") return;
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
  }, []);

  const requestDeleteDmMessage = useCallback(
    (message: Message, options?: { skipConfirm?: boolean }) => {
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
    },
    [handleDeleteDmMessage, openModal],
  );

  // --- Message editing (own DMs only) ---
  const [editingMessageId, setEditingMessageId] = useState<number | null>(null);
  const startEdit = useCallback((message: Message) => {
    if (typeof message.id === "number" && message.id > 0) setEditingMessageId(message.id);
  }, []);
  const cancelEdit = useCallback(() => setEditingMessageId(null), []);
  const submitEdit = useCallback((message: Message, content: string) => {
    const peer = useDmStore.getState().activeDmUser;
    setEditingMessageId(null);
    if (!peer || typeof message.id !== "number" || message.id <= 0) return;
    if (content === message.content) return; // no change → skip
    invoke("edit_dm_message", { peer, messageId: message.id, content }).catch((err) =>
      console.error("edit_dm_message:", err),
    );
  }, []);
  // ArrowUp on an empty composer → edit the latest own message.
  const editLatestOwn = useCallback(() => {
    const peer = useDmStore.getState().activeDmUser;
    if (!peer || !localUsername) return;
    const list = useDmStore.getState().conversations[peer]?.messages ?? [];
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.sender === localUsername && typeof m.id === "number" && m.id > 0) {
        setEditingMessageId(m.id);
        return;
      }
    }
  }, [localUsername]);

  // Type anywhere in the conversation to start composing; ArrowUp edits latest.
  useTypeToFocusComposer(editorRef, editLatestOwn);

  const conversation = activeDmUser
    ? conversations[activeDmUser]
    : null;
  const messages = conversation?.messages ?? [];

  // --- Replies ---
  const [replyingTo, setReplyingTo] = useState<Message | null>(null);
  const startReply = useCallback((m: Message) => {
    if (typeof m.id === "number" && m.id > 0) {
      setReplyingTo(m);
      editorRef.current?.focus();
    }
  }, []);
  const cancelReply = useCallback(() => setReplyingTo(null), []);
  const messagesById = useMemo(() => {
    const map = new Map<number, DmMessage>();
    for (const m of messages) if (typeof m.id === "number" && m.id > 0) map.set(m.id, m);
    return map;
  }, [messages]);
  // Reset transient composer state when switching conversations.
  useEffect(() => {
    setReplyingTo(null);
    setEditingMessageId(null);
  }, [activeDmUser]);

  // Map DmMessages to Message shape for MessageBubble compatibility.
  // Preserve the real server-assigned id when present (persistent-DMs)
  // — the delete flow keys on it. Legacy / synthetic preview entries
  // (pre-persistence DMs) fall back to 0 and the trash icon won't
  // appear for those, which is correct (nothing to delete).
  // Memoized: this used to rebuild (and re-allocate) the whole array
  // on every render — keystrokes included — not just message changes.
  // DmMessage has no nonce, so an unsent bubble has no stable identity
  // of its own; computeItemKey falls back to the index for those. They
  // only ever sit at the tail, so a prepend can't shuffle them.
  const messageKey = useCallback((m: { id: number }) => (m.id > 0 ? m.id : ""), []);

  const bubbleMessages = useMemo(
    () =>
      messages.map((m) => ({
        ...m,
        id: typeof m.id === "number" ? m.id : 0,
        channelId: "",
        attachments: [],
      })),
    [messages],
  );

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

  // Scroll-up paginator — same two-trigger shape as ChatPanel's
  // maybeLoadOlderHistory (see the comment there for the measured
  // group-flip rationale): startReached is the forced hard edge,
  // rangeChanged fires the eager threshold so the page-boundary
  // group-flip resize resolves off-screen.
  const maybeLoadOlderHistory = (force: boolean) => {
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (!conv?.hasMoreHistory) return;
    if (loadMoreInFlightRef.current) return;
    const oldest = conv.messages.find(
      (m): m is typeof m & { id: number } => typeof m.id === "number" && m.id > 0,
    );
    const beforeId = oldest?.id ?? 0;
    if (!force && beforeId !== 0 && beforeId === lastRequestedBeforeIdRef.current) {
      return;
    }
    loadMoreInFlightRef.current = true;
    lastRequestedBeforeIdRef.current = beforeId;
    invoke("request_dm_history", {
      peer: activeDmUser,
      beforeId,
      limit: 50,
    })
      .catch((err) => {
        console.error(err);
        // Allow the eager path to retry this boundary after a failure.
        lastRequestedBeforeIdRef.current = 0;
      })
      .finally(() => {
        loadMoreInFlightRef.current = false;
      });
  };

  // Persist the outgoing peer's scroll state at the moment we leave
  // it. Cleanup runs BEFORE the next setup with the new activeDmUser,
  // so the closure-captured peer is the one we're leaving. Also fires
  // on full unmount (e.g. switching back to the server view) so the
  // position survives view switches and we can restore on return.
  useEffect(() => {
    const peer = activeDmUser;
    return () => {
      if (peer) {
        savedPositionsRef.current[peer] = {
          topIndex: topIndexRef.current,
          atBottom: atBottomRef.current,
        };
      }
    };
  }, [activeDmUser]);

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
    const replyToId = replyingTo?.id && replyingTo.id > 0 ? replyingTo.id : undefined;
    try {
      await invoke("send_private_message", {
        recipient: activeDmUser,
        message: value.trim(),
        replyTo: replyToId,
      });
      editorRef.current?.clear();
      setInput("");
      useDraftsStore.getState().clearDmDraft(activeDmUser);
      setPickerOpen(false);
      setReplyingTo(null);
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

  // See ChatPanel.insertSnippet — same contract.
  const insertSnippet = useCallback((snippet: string) => {
    const cur = editorRef.current?.getValue() ?? "";
    const sep = cur.length > 0 && !cur.endsWith("\n") ? "\n" : "";
    editorRef.current?.focus();
    editorRef.current?.setValue(cur + sep + snippet);
  }, []);

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

  // Initial Virtuoso position when we mount/re-mount for this peer.
  // atBottom: user was caught up — land them at LAST so newer messages
  // received while away are visible. Otherwise restore the saved
  // topmost index. Defensive clamps against eviction.
  // Keeps the viewport anchored when older history pages in at the top.
  const firstItemIndex = useVirtuosoPrepend(bubbleMessages, messageKey, activeDmUser);

  const initialIndex = (() => {
    const last = Math.max(0, bubbleMessages.length - 1);
    if (!activeDmUser) return last;
    const saved = savedPositionsRef.current[activeDmUser];
    if (!saved || saved.atBottom) return last;
    if (saved.topIndex < 0 || saved.topIndex > last) return last;
    return saved.topIndex;
  })();

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      {/* DM header */}
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border px-4">
        <div className="relative">
          <UserAvatar username={activeDmUser} size={26} />
          <div
            className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-tertiary ${
              isOnline ? "bg-success" : "bg-text-muted"
            }`}
          />
        </div>
        <span className="font-display text-title font-emphasis tracking-title text-text-bright">
          {activeDmUser}
        </span>
        {isOnline ? (
          <div className="flex items-center gap-[5px] rounded-sm bg-success/15 px-2 py-0.5 font-channel text-[11px] font-medium text-success">
            <div className="h-1.5 w-1.5 rounded-full bg-success" />
            Online
          </div>
        ) : (
          <div className="flex items-center gap-[5px] rounded-sm bg-text-muted/15 px-2 py-0.5 font-channel text-[11px] font-medium text-text-muted">
            <div className="h-1.5 w-1.5 rounded-full bg-text-muted" />
            Offline
          </div>
        )}
        <div className="flex-1" />
        <div className="flex gap-1">
          <button className="flex h-8 w-8 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" />
            </svg>
          </button>
          <button
            onClick={toggleDmFriendsPanel}
            className={`flex h-8 w-8 items-center justify-center rounded-sm transition-colors ${
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
      {messages.length === 0 ? (
        <div className="flex-1 overflow-y-auto pr-4 py-4">
          <div className="animate-[fadeUp_0.4s_ease_both] pl-4">
            <div className="border-b border-border pb-5 mb-5">
              <UserAvatar username={activeDmUser} size={60} className="mb-3" />
              <h1 className="mb-1.5 text-[26px] font-semibold tracking-tight text-text-bright">
                {activeDmUser}
              </h1>
              <p className="text-sm text-text-secondary leading-[1.55]">
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
        </div>
      ) : (
        <Virtuoso
          // Re-mount when the active peer changes so
          // initialTopMostItemIndex applies fresh — Virtuoso reuses
          // its instance across data swaps and ignores subsequent
          // changes to the initial-position prop.
          key={activeDmUser}
          className="flex-1 pr-4"
          data={bubbleMessages}
          firstItemIndex={firstItemIndex}
          computeItemKey={(index, m) => messageKey(m) || `i:${index}`}
          initialTopMostItemIndex={initialIndex}
          followOutput="smooth"
          // Discord-style: stack messages from the bottom up against
          // the input bar when total content height is below viewport.
          alignToBottom={true}
          // Same rationale as ChatPanel: settle off-screen so rows
          // scroll in already correct.
          increaseViewportBy={{ top: 600, bottom: 600 }}
          startReached={() => maybeLoadOlderHistory(true)}
          rangeChanged={(range) => {
            // Absolute — see ChatPanel.
            const start = range.startIndex - firstItemIndex;
            topIndexRef.current = start;
            // Eager pagination — see ChatPanel's rangeChanged.
            if (start < HISTORY_EAGER_THRESHOLD) maybeLoadOlderHistory(false);
          }}
          atBottomStateChange={(atBottom) => {
            atBottomRef.current = atBottom;
          }}
          itemContent={(absoluteIndex, msg) => {
            // See ChatPanel: firstItemIndex makes this index absolute.
            const i = absoluteIndex - firstItemIndex;
            const isError =
              msg.sender === localUsername &&
              ERROR_MESSAGES.includes(msg.content);
            if (isError) {
              return (
                <div className="pl-4 pr-2 py-1.5">
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
                message={msg}
                grouped={
                  shouldGroup(i > 0 ? bubbleMessages[i - 1] : undefined, msg) &&
                  !msg.replyTo
                }
                paddingLeft={12}
                canDelete={
                  typeof msg.id === "number" &&
                  msg.id > 0 &&
                  msg.sender === localUsername
                }
                onDelete={requestDeleteDmMessage}
                canEdit={
                  typeof msg.id === "number" &&
                  msg.id > 0 &&
                  msg.sender === localUsername
                }
                editing={editingMessageId === msg.id && msg.id > 0}
                onStartEdit={startEdit}
                onSubmitEdit={submitEdit}
                onCancelEdit={cancelEdit}
                canReply={typeof msg.id === "number" && msg.id > 0}
                onReply={startReply}
                replyToSender={
                  msg.replyTo ? messagesById.get(msg.replyTo)?.sender : undefined
                }
                replyToContent={
                  msg.replyTo ? messagesById.get(msg.replyTo)?.content : undefined
                }
              />
            );
          }}
        />
      )}

      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar — py-2 gives an 8px gap above the bar, matching
          ChatPanel's spacing. */}
      <div className="px-3 py-2">
        <div className="relative flex min-h-[54px] flex-col gap-2.5 rounded-lg border border-border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-ring">
          {replyingTo && (
            <div className="flex items-center justify-between gap-2 text-meta text-text-muted">
              <span className="min-w-0 truncate">
                Replying to{" "}
                <span className="font-medium text-text-secondary">@{replyingTo.sender}</span>
              </span>
              <button
                onClick={cancelReply}
                title="Cancel reply"
                className="flex h-5 w-5 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-row-hover hover:text-text-primary"
              >
                <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </div>
          )}
          <MessagePreview draft={input} />
          <div className="flex items-center gap-2.5">
          <RichInput
            ref={editorRef}
            onChange={handleInputChange}
            onEnter={handleSend}
            onArrowUpEmpty={editLatestOwn}
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
            <RichComposer onInsert={insertSnippet} />
            <div className="relative">
              <button
                ref={emojiTriggerRef}
                onClick={() => setPickerOpen((v) => !v)}
                className={`flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-sm transition-colors ${
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
              className="flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-sm bg-accent text-on-accent transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
              </svg>
            </button>
          </div>
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
