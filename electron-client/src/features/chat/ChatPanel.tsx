import { useEffect, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import { pickFiles, ATTACHMENT_FILTERS } from "./filePicker";
import { queueUpload, startQueuedUpload } from "./uploadAttachment";
import { chunkSourceFromPath } from "./chunkSource";
import WelcomeState from "./WelcomeState";

function generateNonce(): string {
  return `n-${Date.now()}-${Math.floor(Math.random() * 1_000_000).toString(36)}`;
}

function generatePendingId(): string {
  return `att-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

// pathsToFiles used to fs.readFile each picked path, materialising the
// whole file in renderer memory. Replaced by per-path ChunkSource
// registration: we hand main the absolute path, get back a
// `decibell-file://<token>` URL + metadata, and stream chunks lazily
// during upload. Bytes never cross IPC as a single buffer.

export default function ChatPanel() {
  const username = useAuthStore((s) => s.username);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const messagesByChannel = useChatStore((s) => s.messagesByChannel);
  const historyLoading = useChatStore((s) => s.historyLoading);
  const dragActive = useUiStore((s) => s.dragActive);
  const dragHoveredKey = useUiStore((s) => s.dragHoveredKey);

  const [draft, setDraft] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const editorRef = useRef<RichInputHandle>(null);
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);
  const chatViewRef = useRef<HTMLDivElement>(null);

  // Track the chat viewport size and publish it to the store so
  // AttachmentList can scale image/video previews proportionally to
  // the available space (sqrt-based, see attachmentSizing.ts). On
  // unmount we clear the size so the helpers fall back to fixed
  // defaults instead of using a stale dimension.
  useEffect(() => {
    const el = chatViewRef.current;
    if (!el) return;
    const setSize = useChatStore.getState().setChatViewSize;
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (!rect) return;
      setSize({ width: rect.width, height: rect.height });
    });
    observer.observe(el);
    // Seed an initial value before the first observer fire — useful
    // for the synchronous first render of attachments below.
    const rect = el.getBoundingClientRect();
    setSize({ width: rect.width, height: rect.height });
    return () => {
      observer.disconnect();
      setSize(null);
    };
  }, []);

  const channels = activeServerId ? channelsByServer[activeServerId] ?? [] : [];
  const channel = channels.find((c) => c.id === activeChannelId) ?? null;
  const channelName = channel?.name ?? activeChannelId ?? null;
  const messages = activeChannelId ? messagesByChannel[activeChannelId] ?? [] : [];
  const loading = activeChannelId ? historyLoading[activeChannelId] === true : false;
  const dropHoveredHere = dragHoveredKey === "active-input";

  // Live "are there any non-failed pendings for this channel" — drives
  // the send button's enabled state. Subscribing via a derived boolean
  // (rather than reading getState() at render time) means the button
  // re-evaluates the moment a queued upload is added or the moment
  // the last pending is removed, no other render trigger required.
  const hasLivePendings = useAttachmentsStore((s) => {
    if (!activeServerId || !activeChannelId) return false;
    for (const p of Object.values(s.pendings)) {
      if (
        p.serverId === activeServerId &&
        p.channelId === activeChannelId &&
        p.status !== "failed"
      ) {
        return true;
      }
    }
    return false;
  });

  // Live scroll-tracking refs — written by Virtuoso's rangeChanged /
  // atBottomStateChange callbacks below, read by the cleanup of the
  // channel-switch effect to persist the OUTGOING channel's position
  // before activeChannelId flips.
  const topIndexRef = useRef(0);
  const atBottomRef = useRef(true);

  // Compute Virtuoso's initialTopMostItemIndex from the channel's
  // saved scroll position. This is the *only* mechanism we use to
  // restore — combined with the `key={activeChannelId}` below, every
  // channel switch unmounts the previous Virtuoso and mounts a fresh
  // one with the right starting position. That sidesteps the racey
  // post-mount `scrollToIndex` we tried first, which Virtuoso would
  // sometimes silently swallow because its own initial render had
  // already moved past it.
  const initialIndex = (() => {
    if (messages.length === 0) return 0;
    const last = messages.length - 1;
    if (!activeChannelId) return last;
    const saved =
      useChatStore.getState().scrollPositionsByChannel[activeChannelId];
    // atBottom: user was caught up — land them at LAST so any messages
    // arrived during the absence are visible.
    if (!saved || saved.atBottom) return last;
    // Defensive clamp against eviction / cache shrink.
    if (saved.topIndex < 0 || saved.topIndex > last) return last;
    return saved.topIndex;
  })();

  // Persist the outgoing channel's scroll state at the moment we leave
  // it. Cleanup runs BEFORE the next setup with the new activeChannelId,
  // so the closure-captured channelId is the one we're leaving. Also
  // fires on full unmount (e.g. switching to DM view) so the position
  // survives view switches and we can restore on return.
  useEffect(() => {
    const channelId = activeChannelId;
    return () => {
      if (channelId) {
        useChatStore
          .getState()
          .setScrollPosition(channelId, topIndexRef.current, atBottomRef.current);
      }
    };
  }, [activeChannelId]);

  // Fetch channel history the first time we land on a channel — covers
  // every entry path (sidebar click, server-tab auto-select, browse-view
  // join, deep link). The previous codepath only fetched on explicit
  // sidebar click, so landing on the auto-selected first text channel
  // when entering a server left the chat empty until the user clicked
  // away and back. Read state via getState() to avoid pulling
  // historyFetched/historyLoading into the subscription set — they
  // change on every history page response and we only need them at
  // effect-fire time.
  useEffect(() => {
    if (!activeServerId || !activeChannelId) return;
    const chat = useChatStore.getState();
    if (chat.historyFetched[activeChannelId] || chat.historyLoading[activeChannelId]) {
      return;
    }
    chat.setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: 0,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history:", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  }, [activeServerId, activeChannelId]);


  // No more pre-computed `bubbles` array. The old code rebuilt it on
  // every messages-reference change — for a 5000-message channel,
  // that's 5000 wrapper-object allocations on every wire message
  // arrival just to surface `grouped`/`isLast` to MessageBubble. The
  // new path passes `messages` directly to Virtuoso and computes
  // grouped/isLast inside itemContent, which Virtuoso only invokes
  // for visible rows (~30 at a time). Saves ~99% of the per-arrival
  // allocation work on long channels without changing visible output.

  const handlePickFiles = async () => {
    if (!activeServerId || !activeChannelId) return;
    const paths = await pickFiles({ multiple: true, filters: ATTACHMENT_FILTERS });
    if (!paths) return;
    for (const p of paths) {
      try {
        const source = await chunkSourceFromPath(p);
        const pendingId = generatePendingId();
        // queueUpload registers the attachment as `queued` only —
        // the actual byte transfer kicks off in handleSend below.
        queueUpload({
          pendingId,
          serverId: activeServerId,
          channelId: activeChannelId,
          source,
        }).catch(() => {});
      } catch (e) {
        console.error("file register:", p, e);
      }
    }
  };

  const handleSend = async () => {
    const content = (editorRef.current?.getValue() ?? "").trim();
    if (!activeServerId || !activeChannelId || !username) return;

    const channelPendings = useAttachmentsStore
      .getState()
      .selectForChannel(activeServerId, activeChannelId);
    if (!content && channelPendings.length === 0) return;

    const livePendings = channelPendings.filter((p) => p.status !== "failed");
    const pendingIds = livePendings.map((p) => p.pendingId);

    const nonce = generateNonce();
    useChatStore.getState().addMessage({
      id: 0,
      channelId: activeChannelId,
      sender: username,
      content,
      timestamp: String(Math.floor(Date.now() / 1000)),
      attachments: [],
      nonce,
      pendingAttachmentIds: pendingIds.length > 0 ? pendingIds : undefined,
    });
    editorRef.current?.clear();
    setDraft("");

    // Kick off the actual byte transfer for every queued attachment.
    // queueUpload registered them with status "queued" at file-pick /
    // drop / paste time but didn't send any bytes — we wait for
    // explicit user intent (this send) before touching the network.
    // Failed uploads are skipped (their pendingId stayed in the
    // optimistic bubble's pendingAttachmentIds list, so the bubble
    // shows them as failed via BubbleInflightAttachments).
    for (const id of pendingIds) {
      const p = useAttachmentsStore.getState().pendings[id];
      if (p && p.status === "queued") {
        startQueuedUpload(id).catch(() => {
          // Errors are surfaced via the store's markFailed → the
          // BubbleInflightAttachments row picks it up.
        });
      }
    }

    const waitForUploads = async (): Promise<number[]> => {
      const ids: number[] = [];
      while (true) {
        const current = pendingIds
          .map((id) => useAttachmentsStore.getState().pendings[id])
          .filter((p): p is NonNullable<typeof p> => Boolean(p));
        if (current.every((p) => p.status === "ready" || p.status === "failed")) {
          for (const p of current) {
            if (p.status === "ready" && p.attachmentId !== null) {
              ids.push(p.attachmentId);
            }
          }
          return ids;
        }
        await new Promise((r) => setTimeout(r, 100));
      }
    };

    let attachmentIds: number[] = [];
    if (pendingIds.length > 0) {
      attachmentIds = await waitForUploads();
    }

    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: content,
        attachmentIds: attachmentIds.length > 0 ? attachmentIds : undefined,
        nonce,
      });
    } catch (err) {
      console.error("send_channel_message:", err);
    }

    for (const id of pendingIds) {
      useAttachmentsStore.getState().removePending(id);
    }
  };

  // Suppress the input wrapper's default drop-handling so dragged
  // files don't appear as a path string in the message input —
  // useDragDrop at the window level handles the upload.
  const suppressDrop = (e: React.DragEvent<HTMLElement>) => {
    e.preventDefault();
  };

  if (!activeServerId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
        Pick a server to start chatting.
      </div>
    );
  }

  if (!activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
        Pick a channel.
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      <div className="flex h-12 items-center border-b border-border-divider px-4 text-sm font-semibold text-text-bright">
        <span className="mr-1.5 text-text-muted">#</span>
        {channelName}
      </div>

      <div ref={chatViewRef} className="flex flex-1 flex-col">
        {loading && messages.length === 0 ? (
          <div className="flex flex-1 items-center justify-center text-sm text-text-muted">
            Loading history…
          </div>
        ) : messages.length === 0 ? (
          <div className="flex flex-1 items-center justify-center">
            <WelcomeState channelName={channelName ?? "channel"} />
          </div>
        ) : (
          <Virtuoso
            // Re-mount Virtuoso whenever the active channel changes
            // so initialTopMostItemIndex applies fresh — without the
            // key, Virtuoso reuses its instance across data swaps and
            // ignores any change to the initial-position prop.
            key={activeChannelId ?? "none"}
            ref={virtuosoRef}
            data={messages}
            initialTopMostItemIndex={initialIndex}
            followOutput="smooth"
            rangeChanged={(range) => {
              topIndexRef.current = range.startIndex;
            }}
            atBottomStateChange={(atBottom) => {
              atBottomRef.current = atBottom;
            }}
            itemContent={(index, message) => (
              <MessageBubble
                message={message}
                grouped={shouldGroup(
                  index > 0 ? messages[index - 1] : undefined,
                  message,
                )}
                serverId={activeServerId}
                isLast={index === messages.length - 1}
              />
            )}
            className="flex-1"
          />
        )}
      </div>

      {/* Input bar — pending attachments live INSIDE the rounded chrome
          so adding files visibly expands the bar upward (Discord pattern).
          Drop-target wiring: the bar lights up while a drag is in flight,
          saturated state when the cursor is over the bar. */}
      <div className="px-3 py-2" data-drop-target="active-input">
        <div
          onDragOver={suppressDrop}
          onDrop={suppressDrop}
          className={`relative flex min-h-[54px] flex-col gap-2.5 rounded-xl border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)] ${
            dropHoveredHere
              ? "border-accent bg-accent-soft/50 animate-[dropTargetIn_0.18s_ease_both]"
              : dragActive
                ? "border-transparent animate-[dropPulse_1.6s_ease-in-out_infinite]"
                : "border-border"
          }`}
        >
          <PendingAttachmentsRow />
          <div className="flex items-center gap-2.5">
            {dropHoveredHere && (
              <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-xl bg-gradient-to-b from-accent-soft/80 to-accent-soft/40 backdrop-blur-[3px]">
                <svg
                  width="20" height="20" viewBox="0 0 24 24" fill="none"
                  stroke="currentColor" strokeWidth="2" strokeLinecap="round"
                  strokeLinejoin="round"
                  className="animate-[dropTargetIn_0.18s_ease_both] text-accent-bright"
                >
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                  <polyline points="17 8 12 3 7 8" />
                  <line x1="12" y1="3" x2="12" y2="15" />
                </svg>
                <span className="text-[13px] font-semibold text-accent-bright">
                  Drop to upload to #{channelName ?? "channel"}
                </span>
              </div>
            )}
            <button
              onClick={handlePickFiles}
              title="Attach files"
              className="flex h-[34px] w-[34px] shrink-0 self-end items-center justify-center rounded-lg bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="12" y1="8" x2="12" y2="16" />
                <line x1="8" y1="12" x2="16" y2="12" />
              </svg>
            </button>
            <RichInput
              ref={editorRef}
              onChange={setDraft}
              onEnter={handleSend}
              placeholder={`Message #${channelName ?? "channel"}`}
              maxHeight={160}
              className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            />
            <div className="flex shrink-0 self-end gap-1">
              <div className="relative">
                <button
                  ref={emojiTriggerRef}
                  onClick={() => setPickerOpen((v) => !v)}
                  title="Emoji"
                  className={`flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-md transition-colors ${
                    pickerOpen
                      ? "bg-surface-hover text-text-secondary"
                      : "text-text-muted hover:text-text-secondary"
                  }`}
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <circle cx="12" cy="12" r="10" />
                    <path d="M8 14s1.5 2 4 2 4-2 4-2" />
                    <line x1="9" y1="9" x2="9.01" y2="9" />
                    <line x1="15" y1="9" x2="15.01" y2="9" />
                  </svg>
                </button>
                {pickerOpen && (
                  <EmojiPicker
                    onSelect={(emoji) => {
                      editorRef.current?.insertEmoji(emoji);
                      editorRef.current?.focus();
                    }}
                    onClose={() => setPickerOpen(false)}
                    triggerRef={emojiTriggerRef}
                  />
                )}
              </div>
              <button
                onClick={handleSend}
                disabled={!draft.trim() && !hasLivePendings}
                className="flex h-[34px] w-[34px] shrink-0 items-center justify-center rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
                title="Send"
              >
                <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
