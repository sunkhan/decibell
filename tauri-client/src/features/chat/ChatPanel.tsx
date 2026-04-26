import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { pickFiles } from "./filePicker";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { toast } from "../../stores/toastStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";
import WelcomeState from "./WelcomeState";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import { uploadAttachment, startQueuedUpload } from "./uploadAttachment";

// Must match the INITIAL_FIRST_INDEX seed in chatStore.prependHistory. Keeps
// Virtuoso's firstItemIndex anchored well above zero so any realistic amount
// of prepending still leaves room to decrement.
const INITIAL_FIRST_INDEX = 1_000_000_000;

// Stable key for a message (server id when known; ephemeral tuple otherwise).
// Shared between Virtuoso's computeItemKey and the grouping memo so the two
// stay in lockstep.
function messageKey(
  message: { id: number; timestamp: string; sender: string },
  fallbackIndex: number,
): string {
  return message.id !== 0
    ? String(message.id)
    : `eph-${message.timestamp}-${message.sender}-${fallbackIndex}`;
}

// Inner Virtuoso wrapper. Mounts only once messages are non-empty (parent
// gates this) and has key={channelId}, so this component's lifetime equals
// "one Virtuoso instance for one channel visit". That lets us capture
// initialTopMostItemIndex into useState's lazy initializer at first render
// and have it stay stable thereafter — Virtuoso then doesn't re-apply it
// on each prepend, which was fighting with firstItemIndex anchoring and
// causing the scroll-up stutter.
function MessagesView({
  channelId,
  serverId,
  channelName,
  messages,
  firstItemIndex,
  hasMoreHistory,
  historyLoading,
  fetchOlderPage,
}: {
  channelId: string;
  serverId: string | null;
  channelName: string | null;
  messages: import("../../types").Message[];
  firstItemIndex: number;
  hasMoreHistory: boolean;
  historyLoading: boolean;
  fetchOlderPage: () => void;
}) {
  const virtuosoRef = useRef<VirtuosoHandle>(null);

  // "Jump to present" button visibility. Shown when more than 50 messages
  // sit below the user's current viewport — the rangeChanged callback
  // updates this whenever scroll changes the visible range. setState
  // bails out when the boolean is unchanged, so this doesn't churn on
  // every scroll frame.
  const [showJumpToBottom, setShowJumpToBottom] = useState(false);

  // Pull this channel's saved scroll snapshot (if any) once, at mount.
  // We deliberately don't subscribe — the snapshot is consumed exactly
  // once by `restoreStateFrom` and any subsequent updates would just
  // cause a useless re-render.
  const restoredState = useRef(
    useChatStore.getState().channelScrollState[channelId],
  ).current;

  // Save the Virtuoso state into the store every time the user pauses
  // scrolling. We can't rely on an unmount-time save: when `key` changes
  // on this component, the *new* MessagesView renders (and reads the
  // saved state) before React runs the old one's cleanup, so a save in
  // cleanup arrives too late for the immediately-next visit. Plus
  // `getState` is async — its callback may not even fire before the
  // remount. Saving on every scroll-stop keeps the store in sync with
  // the latest position so any subsequent visit can restore cleanly.
  const saveScrollState = (scrolling: boolean) => {
    if (scrolling) return;
    virtuosoRef.current?.getState((state) => {
      useChatStore.getState().setChannelScrollState(channelId, state);
    });
  };

  // Warm-up phase: briefly disable top virtualization so every loaded
  // row mounts and Virtuoso measures it. Once heights are cached and
  // we re-engage virtualization, previously-measured rows mount and
  // unmount silently on scroll — no scroll-position compensation,
  // no stutter.
  //
  // Two triggers invalidate Virtuoso's height cache for unmounted rows:
  //   1. Prepend (firstItemIndex changes) — new rows have no cached size.
  //   2. Resize (chatViewSize changes) — image previews adopt new
  //      dimensions, so rows that were unmounted during the resize have
  //      stale cached heights and would stutter on scroll-back-to.
  //
  // Resize is debounced ~200 ms so a drag-resize doesn't keep retriggering
  // the warm-up while the user is still dragging. Once the size stops
  // changing, a 500 ms warm-up runs and re-measures every row at the
  // new size.
  const chatViewSize = useChatStore((s) => s.chatViewSize);
  const sizeKey = chatViewSize
    ? `${Math.round(chatViewSize.width)}x${Math.round(chatViewSize.height)}`
    : "";
  const [stableSizeKey, setStableSizeKey] = useState("");
  useEffect(() => {
    if (sizeKey === stableSizeKey) return;
    const id = setTimeout(() => setStableSizeKey(sizeKey), 200);
    return () => clearTimeout(id);
  }, [sizeKey, stableSizeKey]);

  const warmKey = `${firstItemIndex}|${stableSizeKey}`;
  const [warmedKey, setWarmedKey] = useState<string | null>(null);
  useEffect(() => {
    if (warmedKey === warmKey) return;
    const id = setTimeout(() => setWarmedKey(warmKey), 500);
    return () => clearTimeout(id);
  }, [warmKey, warmedKey]);
  const isWarmingUp = warmedKey !== warmKey;

  // Imperative scroll to the newest message right after Virtuoso mounts.
  // Skipped when we have a saved snapshot — `restoreStateFrom` will land
  // us at the previous scroll position instead.
  useEffect(() => {
    if (restoredState) return;
    if (messages.length === 0) return;
    const id = setTimeout(() => {
      virtuosoRef.current?.scrollToIndex({
        index: messages.length - 1,
        align: "end",
        behavior: "auto",
      });
    }, 0);
    return () => clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const groupedByMessageKey = useMemo(() => {
    const map = new Map<string, boolean>();
    for (let i = 0; i < messages.length; i++) {
      const key = messageKey(messages[i], i);
      map.set(key, i > 0 ? shouldGroup(messages[i - 1], messages[i]) : false);
    }
    return map;
  }, [messages]);

  const jumpToBottom = () => {
    setShowJumpToBottom(false);
    virtuosoRef.current?.scrollToIndex({
      index: messages.length - 1,
      align: "end",
      behavior: "smooth",
    });
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
    <Virtuoso
      ref={virtuosoRef}
      data={messages}
      firstItemIndex={firstItemIndex}
      restoreStateFrom={restoredState}
      // Estimate of an average chat row's height. Without this hint
      // Virtuoso assumes ~32px and has to reconcile its scroll offset
      // against the real measured height every time a row mounts during
      // scroll — the dominant per-frame cost on long histories. Seeding
      // with a value close to truth (header/avatar row ~70px, grouped
      // body row ~40px → ~64 average) keeps that compensation small.
      defaultItemHeight={64}
      followOutput="auto"
      // `atBottomThreshold` defines the pixel window near the bottom in
      // which Virtuoso still considers us "at bottom". The default (~4px)
      // is too tight — during rapid message spam, momentary scroll drift
      // between appends flips state to "not at bottom" and followOutput
      // stops tracking, so new messages pile up just off-screen. 150px
      // absorbs that jitter without feeling sticky.
      atBottomThreshold={150}
      isScrolling={saveScrollState}
      // During warm-up, top overscan is unbounded so every loaded row
      // mounts and gets measured. Once heights are cached we drop back
      // to a small overscan and let Virtuoso virtualize normally —
      // virtualizing measured rows is silent (no scroll-position
      // compensation), which is what makes scroll-up stay smooth while
      // most rows are unmounted to free memory.
      increaseViewportBy={{
        top: isWarmingUp ? Number.MAX_SAFE_INTEGER : 3000,
        bottom: 400,
      }}
      startReached={fetchOlderPage}
      rangeChanged={({ startIndex, endIndex }) => {
        // Fire the next-page fetch well before the user reaches the top
        // so the network roundtrip overlaps with their scrolling rather
        // than blocking it.
        const position = startIndex - firstItemIndex;
        if (position <= 30) fetchOlderPage();

        // Show the jump-to-bottom button once more than 50 messages sit
        // below the visible window. setState bails on unchanged values
        // so this is cheap on every-frame fires.
        const lastVisible = endIndex - firstItemIndex;
        const messagesBelow = messages.length - 1 - lastVisible;
        setShowJumpToBottom(messagesBelow > 50);
      }}
      computeItemKey={(index, message) => {
        if (!message) return `idx-${index}`;
        const position = index - firstItemIndex;
        return messageKey(message, position);
      }}
      components={{
        Header: () => {
          if (historyLoading) {
            return (
              <div className="flex justify-center py-2 text-[11px] text-text-muted">
                Loading older messages…
              </div>
            );
          }
          if (hasMoreHistory === false) {
            return (
              <div className="flex justify-center py-2 text-[11px] text-text-muted">
                Start of #{channelName ?? "channel"}
              </div>
            );
          }
          return null;
        },
      }}
      itemContent={(index, message) => {
        const position = index - firstItemIndex;
        const grouped =
          groupedByMessageKey.get(messageKey(message, position)) ?? false;
        const isLast = position === messages.length - 1;
        return (
          <div className="px-4">
            <MessageBubble
              message={message}
              grouped={grouped}
              serverId={serverId}
              isLast={isLast}
            />
          </div>
        );
      }}
      style={{ flex: 1, minHeight: 0 }}
    />
    {showJumpToBottom && (
      <button
        onClick={jumpToBottom}
        title="Jump to present"
        className="absolute bottom-3 right-4 z-10 flex cursor-pointer items-center gap-1.5 rounded-xl bg-accent px-3 py-2 text-[12px] font-medium text-white shadow-[0_4px_16px_var(--color-accent-soft)] transition-all hover:bg-accent-hover active:scale-95 animate-[fadeUp_0.18s_ease_both]"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="6 9 12 15 18 9" />
        </svg>
        Jump to present
      </button>
    )}
    </div>
  );
}

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
  const firstItemIndexByChannel = useChatStore((s) => s.firstItemIndexByChannel);
  const activeView = useUiStore((s) => s.activeView);
  const dragActive = useUiStore((s) => s.dragActive);
  const dragHoveredKey = useUiStore((s) => s.dragHoveredKey);
  const dropHoveredHere = dragHoveredKey === "active-input";

  const [input, setInput] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);

  // True when at least one pending attachment is in a sendable state
  // (queued or ready). Lets the send button light up even when the
  // text input is empty, mirroring handleSend's own gating logic.
  // Subscribing as a primitive bool means the button only re-renders
  // when the flag flips, not on every progress tick.
  const hasSendableAttachments = useAttachmentsStore((s) => {
    if (!activeChannelId) return false;
    const order = s.orderByChannel[activeChannelId] ?? [];
    for (const id of order) {
      const a = s.byPendingId[id];
      if (a && (a.status === "queued" || a.status === "ready")) return true;
    }
    return false;
  });

  // Publish the messages-area dimensions to the store so image previews
  // can size against them. Implemented as a *callback ref* (not
  // useEffect on a useRef) because ChatPanel early-returns in home/DM
  // view, so the messages container only exists conditionally — a
  // useEffect with [] deps would never see the element if first mount
  // was in the empty-state path. Callback refs fire whenever the
  // observed element is attached or detached, so the ResizeObserver
  // also attaches/detaches accordingly. Updates are RAF-throttled to
  // avoid re-rendering every visible image bubble at frame rate during
  // a drag-resize.
  const observerRef = useRef<ResizeObserver | null>(null);
  const measureFrameRef = useRef(0);
  useEffect(() => {
    return () => {
      observerRef.current?.disconnect();
      observerRef.current = null;
      cancelAnimationFrame(measureFrameRef.current);
      useChatStore.getState().setChatViewSize(null);
    };
  }, []);
  const setMessagesContainerRef = useCallback((el: HTMLDivElement | null) => {
    observerRef.current?.disconnect();
    observerRef.current = null;
    cancelAnimationFrame(measureFrameRef.current);
    if (!el) {
      useChatStore.getState().setChatViewSize(null);
      return;
    }
    const measure = () => {
      cancelAnimationFrame(measureFrameRef.current);
      measureFrameRef.current = requestAnimationFrame(() => {
        const r = el.getBoundingClientRect();
        useChatStore.getState().setChatViewSize({
          width: r.width,
          height: r.height,
        });
      });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    observerRef.current = ro;
  }, []);

  const messages = activeChannelId
    ? messagesByChannel[activeChannelId] ?? []
    : [];

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  // Virtuoso's sliding index for the oldest currently-loaded item. Decrements
  // on each prepend (managed in chatStore.prependHistory) so the virtualizer
  // can anchor the visible row without our own pre-scroll measurement dance.
  const firstItemIndex = activeChannelId
    ? firstItemIndexByChannel[activeChannelId] ?? INITIAL_FIRST_INDEX
    : INITIAL_FIRST_INDEX;

  // Bottom-start on channel open is now handled by MessagesView's lazy
  // useState capture of initialTopMostItemIndex — see that component for
  // the rationale. The previous imperative scrollToIndex + memoized
  // grouping logic moved there.

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
      limit: 150,
    }).catch((err) => {
      console.error("request_channel_history failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  }, [activeServerId, activeChannelId, historyFetched]);

  // Background-chain history pages until we have a healthy buffer (~1000
  // messages) loaded for the active channel.
  //
  // Why: each prepend grows scrollHeight and forces Virtuoso to re-anchor
  // the visible content (firstItemIndex compensation), which "fights" any
  // active wheel/drag scroll the user is performing — the scrollbar thumb
  // appears to lag because the list is growing as fast as they scroll.
  // The cleanest fix is to load enough upfront that typical scroll-up
  // sessions never trigger another prepend. 1000 messages covers nearly
  // all realistic chat sessions; channels larger than that fall back to
  // the existing on-demand pagination, which is the right tradeoff.
  //
  // Implementation note: this effect re-runs after every prepend (because
  // `messages` reference changes), naturally chaining the next fetch
  // until either the cap is hit or the server says no more history.
  // historyLoading guards prevent overlap with the user's own scroll-up
  // fetches.
  const PREFETCH_TARGET = 1000;
  useEffect(() => {
    if (!activeServerId || !activeChannelId) return;
    if (messages.length === 0) return;
    if (messages.length >= PREFETCH_TARGET) return;
    if (!hasMoreHistory[activeChannelId]) return;
    if (historyLoading[activeChannelId]) return;
    const firstId = messages.find((m) => m.id !== 0)?.id ?? 0;
    if (firstId === 0) return;
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: firstId,
      limit: 100,
    }).catch((err) => {
      console.error("background history prefetch failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  }, [activeServerId, activeChannelId, messages, hasMoreHistory, historyLoading]);

  // Virtuoso calls this whenever the top of the list scrolls into view
  // (including, helpfully, when the initial page doesn't fill the viewport
  // so there's no scrollbar to interact with). Our own guards prevent
  // double-fires while a request is already in flight.
  const fetchOlderPage = () => {
    if (!activeServerId || !activeChannelId) return;
    if (!hasMoreHistory[activeChannelId]) return;
    if (historyLoading[activeChannelId]) return;
    const firstId = messages.find((m) => m.id !== 0)?.id ?? 0;
    if (firstId === 0) return;
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: firstId,
      limit: 100,
    }).catch((err) => {
      console.error("request_channel_history (paginate) failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId!, false);
    });
  };

  // Auto-focus editor when user starts typing anywhere
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key.length !== 1) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (target?.isContentEditable) return;
      // Skip when the user is interacting with a video player (focused
      // wrapper carries `data-video-player`). Otherwise pressing Space
      // to pause/play would steal focus into the chat composer.
      if (target?.closest("[data-video-player]")) return;
      editorRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleSend = async () => {
    const value = editorRef.current?.getValue() ?? input;
    if (!activeServerId || !activeChannelId) return;

    const channelId = activeChannelId;
    const serverId = activeServerId;
    const pending = useAttachmentsStore.getState().selectForChannel(channelId);
    const queuedItems = pending.filter((a) => a.status === "queued");
    const readyItems = pending.filter((a) => a.status === "ready" && a.attachmentId);
    const failedOrCancelled = pending.filter(
      (a) => a.status === "failed" || a.status === "cancelled",
    );
    const trimmed = value.trim();

    if (!trimmed && queuedItems.length === 0 && readyItems.length === 0) return;

    if (failedOrCancelled.length > 0) {
      toast.error(
        "Some attachments need attention",
        "Remove the failed/cancelled attachments before sending.",
      );
      return;
    }

    // Snapshot the pendings that belong to *this* send. Marking the queued
    // ones `uploading` immediately means a subsequent handleSend (e.g.,
    // the user hits send again with new attachments while this batch is
    // still uploading) won't re-snapshot them — its own pending filter
    // skips items that aren't queued/ready.
    for (const q of queuedItems) {
      useAttachmentsStore.getState().markUploading(q.pendingId);
    }
    const snapshotIds = new Set([
      ...queuedItems.map((p) => p.pendingId),
      ...readyItems.map((p) => p.pendingId),
    ]);

    // Clear the composer immediately so the user can keep typing/sending.
    editorRef.current?.clear();
    setInput("");
    setSendError(null);
    useDraftsStore.getState().clearChannelDraft(channelId);
    setPickerOpen(false);

    // Background task — upload, send, then remove this snapshot's items
    // from the pending row. Errors land in toasts since the inline red
    // text below the messages would be invisible by the time we hit them.
    void (async () => {
      try {
        if (queuedItems.length > 0) {
          const results = await Promise.all(queuedItems.map((q) => startQueuedUpload(q)));
          if (results.some((r) => !r.ok)) {
            toast.error(
              "Upload failed",
              "Couldn't upload one or more attachments. Remove them and try again.",
            );
            return;
          }
        }

        const finalPending = useAttachmentsStore.getState().selectForChannel(channelId);
        const readyIds = finalPending
          .filter(
            (a) =>
              snapshotIds.has(a.pendingId) &&
              a.status === "ready" &&
              a.attachmentId,
          )
          .map((a) => a.attachmentId as number);

        await invoke("send_channel_message", {
          serverId,
          channelId,
          message: trimmed,
          attachmentIds: readyIds,
        });

        for (const id of snapshotIds) {
          useAttachmentsStore.getState().removePending(id);
        }
      } catch (err) {
        toast.error("Send failed", String(err));
      }
    })();
  };

  // --- File picker + upload kick-off -------------------------------------

  const serverAttachmentConfig = useChatStore((s) => s.serverAttachmentConfig);

  const handleAttach = async () => {
    if (!activeServerId || !activeChannelId) return;
    const cfg = serverAttachmentConfig[activeServerId];
    if (!cfg || cfg.port === 0) {
      toast.error("Attachments unavailable", "This server does not accept file uploads.");
      return;
    }
    let picked: string[] = [];
    try {
      picked = await pickFiles({ title: "Choose files to attach", multiple: true });
    } catch (err) {
      toast.error("File picker failed", String(err));
      return;
    }
    if (picked.length === 0) return;

    for (const filePath of picked) {
      await startUpload(filePath);
    }

    // Hand focus back to the editor so a follow-up Enter sends the
    // message instead of re-firing the attach button (which holds
    // focus after the file dialog closes).
    editorRef.current?.focus();
  };

  const startUpload = async (filePath: string) => {
    if (!activeServerId || !activeChannelId) return;
    const cfg = serverAttachmentConfig[activeServerId];
    // uploadAttachment surfaces its own errors via toast; we only need to
    // await it to keep the for-loop in handleAttach sequential.
    await uploadAttachment({
      filePath,
      serverId: activeServerId,
      channelId: activeChannelId,
      maxBytes: cfg?.maxBytes ?? 0,
    });
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

  // Empty-state short-circuits
  if (activeView === "home" || !activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid">
        <p className="text-sm text-text-muted">
          Select a channel to start chatting
        </p>
      </div>
    );
  }

  const isEmpty = messages.length === 0;
  const isLoadingFirstPage =
    isEmpty &&
    (historyLoading[activeChannelId] ||
      !historyFetched[activeChannelId]);

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      {/* Channel header */}
      {!hideHeader && <ChatHeader />}

      {/* Messages */}
      <div ref={setMessagesContainerRef} className="flex min-h-0 flex-1 flex-col">
        {isEmpty ? (
          <div className="flex flex-1 items-center justify-center">
            {isLoadingFirstPage ? (
              <span className="text-[12px] text-text-muted">Loading…</span>
            ) : (
              <WelcomeState channelName={channelName ?? "channel"} />
            )}
          </div>
        ) : (
          <MessagesView
            // key={channelId} ensures a fresh Virtuoso instance per
            // channel — important for `restoreStateFrom` since that prop
            // is only consumed at mount.
            key={activeChannelId}
            channelId={activeChannelId}
            serverId={activeServerId}
            channelName={channelName ?? null}
            messages={messages}
            firstItemIndex={firstItemIndex}
            hasMoreHistory={hasMoreHistory[activeChannelId] === true}
            historyLoading={!!historyLoading[activeChannelId]}
            fetchOlderPage={fetchOlderPage}
          />
        )}
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar. Pending attachments live inside the same rounded
          chrome (above the textarea row) so adding files visibly
          expands the bar upward, matching Discord's pattern. The bar
          itself is a flex column: pending tiles row on top, controls
          row on bottom.
          Drop-target wiring: when a file drag enters the window, the
          input bar lights up; the more saturated state kicks in when
          the cursor is actually over it. */}
      <div className="px-3 py-2" data-drop-target="active-input">
        <div
          className={`relative flex min-h-[54px] flex-col gap-2.5 rounded-xl border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)] ${
            dropHoveredHere
              ? "border-accent bg-accent-soft/50 animate-[dropTargetIn_0.18s_ease_both]"
              : dragActive
                ? "border-transparent animate-[dropPulse_1.6s_ease-in-out_infinite]"
                : "border-border"
          }`}
        >
          {activeChannelId && <PendingAttachmentsRow channelId={activeChannelId} />}
          <div className="flex items-center gap-2.5">
          {dropHoveredHere && (
            <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-xl bg-gradient-to-b from-accent-soft/80 to-accent-soft/40 backdrop-blur-[3px]">
              <svg
                width="20"
                height="20"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-accent-bright animate-[dropTargetIn_0.18s_ease_both]"
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
            onClick={handleAttach}
            title="Attach files"
            className="flex h-7 w-7 shrink-0 self-end items-center justify-center rounded-full bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="12" cy="12" r="10" /><line x1="12" y1="8" x2="12" y2="16" /><line x1="8" y1="12" x2="16" y2="12" />
            </svg>
          </button>
          <RichInput
            ref={editorRef}
            onChange={handleInputChange}
            onEnter={handleSend}
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
              disabled={!input.trim() && !hasSendableAttachments}
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
    </div>
  );
}
