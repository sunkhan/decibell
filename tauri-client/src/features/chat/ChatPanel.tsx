import { useState, useRef, useEffect, useLayoutEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";
import WelcomeState from "./WelcomeState";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import { kindFromMime, formatBytes } from "./attachmentHelpers";

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
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesContentRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);
  // Scroll-coordination refs. These drive the useLayoutEffect below that
  // decides how to move scrollTop after the message list changes.
  //   prevChannel:      last channel we rendered — a mismatch means "jump to bottom".
  //   prevFirstId:      first (oldest) rendered id — a smaller new value means
  //                     older messages just prepended.
  //   prevMessageCount: so we can tell append from no-op.
  //   wasNearBottom:    sticky-follow heuristic: only auto-scroll on append if
  //                     the user was already within ~80px of the bottom.
  //   pendingPrependAnchor: captured by the scroll handler right before firing
  //                     a user-initiated older-page request, so the layout
  //                     effect can preserve the visual anchor.
  const prevChannelRef = useRef<string | null>(null);
  const prevFirstIdRef = useRef<number>(0);
  const prevMessageCountRef = useRef<number>(0);
  const wasNearBottomRef = useRef<boolean>(true);
  const pendingPrependAnchor = useRef<{ prevSH: number; prevST: number } | null>(null);

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
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history (paginate) failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId!, false);
    });
  };

  const handleScroll = () => {
    const el = messagesContainerRef.current;
    if (!el) return;
    // Update the sticky-follow heuristic on every scroll event. The layout
    // effect reads this after append to decide whether to auto-scroll.
    wasNearBottomRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    // Near-top: user wants older history. Capture current scroll state so the
    // layout effect can preserve the visual anchor after prepend, then fire.
    if (el.scrollTop <= 100 && activeChannelId && hasMoreHistory[activeChannelId] && !historyLoading[activeChannelId]) {
      pendingPrependAnchor.current = { prevSH: el.scrollHeight, prevST: el.scrollTop };
      fetchOlderPage();
    }
  };

  // Viewport-fill auto-paginate: when the first page doesn't overflow the
  // container there's no scrollbar, so scroll-based pagination never fires.
  // Pull the next page silently until the viewport is filled or hasMore=false.
  //
  // Deps: only `messages.length` (along with the active channel/server). The
  // hasMoreHistory and historyLoading Records change reference on *every*
  // store action against them, and depending on those would re-fire this
  // effect mid-flight before its own setHistoryLoading(true) settles —
  // which under bad timing can rapidly nest setStates and trip React's
  // "Maximum update depth exceeded" guard. Reading the latest values via
  // .getState() inside keeps the guard checks accurate without making the
  // effect itself fan out.
  useEffect(() => {
    if (!activeServerId || !activeChannelId) return;
    const ch = activeChannelId;
    const s = useChatStore.getState();
    if (!s.hasMoreHistory[ch]) return;
    if (s.historyLoading[ch]) return;
    const el = messagesContainerRef.current;
    if (!el) return;
    if (el.scrollHeight <= el.clientHeight + 10) {
      fetchOlderPage();
    }
  }, [messages.length, activeServerId, activeChannelId]);  // eslint-disable-line react-hooks/exhaustive-deps

  // Scroll coordination. Runs synchronously after every render but before
  // paint, so the user never sees an intermediate frame at the wrong position.
  useLayoutEffect(() => {
    const el = messagesContainerRef.current;
    if (!el) return;

    const channelChanged = prevChannelRef.current !== activeChannelId;
    const firstId = messages.find((m) => m.id !== 0)?.id ?? 0;
    const prependDetected =
      !channelChanged &&
      firstId !== 0 &&
      prevFirstIdRef.current !== 0 &&
      firstId < prevFirstIdRef.current;
    const appendDetected =
      !channelChanged &&
      !prependDetected &&
      messages.length > prevMessageCountRef.current;

    if (channelChanged) {
      // Instant jump to newest — no animation, no flash.
      el.scrollTop = el.scrollHeight;
      wasNearBottomRef.current = true;
    } else if (prependDetected) {
      if (pendingPrependAnchor.current) {
        // User-initiated scroll-to-top: keep the visible message under the
        // same viewport y-offset by shifting scrollTop by the added height.
        const { prevSH, prevST } = pendingPrependAnchor.current;
        el.scrollTop = el.scrollHeight - prevSH + prevST;
        pendingPrependAnchor.current = null;
      } else {
        // Auto-fill prepend (viewport wasn't yet scrollable) — keep the user
        // pinned to the newest content rather than the newly-revealed oldest.
        el.scrollTop = el.scrollHeight;
      }
    } else if (appendDetected && wasNearBottomRef.current) {
      // New real-time message arrived while the user was at/near the bottom.
      el.scrollTop = el.scrollHeight;
    }
    // If append happened while user was scrolled up reading history, do
    // nothing — their position stays where it was.

    prevChannelRef.current = activeChannelId;
    prevFirstIdRef.current = firstId;
    prevMessageCountRef.current = messages.length;
  }, [activeChannelId, messages.length, messages[0]?.id]);  // eslint-disable-line react-hooks/exhaustive-deps

  // Stay-at-bottom for asynchronous content growth.
  //
  // Image attachments (and potentially anything else that loads after
  // mount — fonts, future embedded media, etc.) render at one height,
  // then expand once their underlying data lands. The append-time
  // useLayoutEffect above already ran with the smaller height, so a
  // message ending in something that grows would leave the user a few
  // hundred px short of the actual bottom.
  //
  // ResizeObserver on the inner content div catches every size change in
  // one place, regardless of cause — much more reliable than chasing
  // individual async events. Whenever the content gets taller and the
  // user was within the near-bottom threshold, pin to the new bottom.
  useEffect(() => {
    const container = messagesContainerRef.current;
    const content = messagesContentRef.current;
    if (!container || !content) return;
    let lastHeight = content.scrollHeight;
    const ro = new ResizeObserver(() => {
      const newHeight = content.scrollHeight;
      if (newHeight === lastHeight) return;
      const grew = newHeight > lastHeight;
      lastHeight = newHeight;
      if (!grew) return;
      if (!wasNearBottomRef.current) return;
      // requestAnimationFrame ensures the browser has applied the new
      // layout before we read scrollHeight on the container.
      requestAnimationFrame(() => {
        if (!messagesContainerRef.current) return;
        messagesContainerRef.current.scrollTop = messagesContainerRef.current.scrollHeight;
      });
    });
    ro.observe(content);
    return () => ro.disconnect();
  }, []);

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
    if (!activeServerId || !activeChannelId) return;

    // Pull pending attachments for this channel. Sending is blocked unless
    // every one is in a terminal state — we refuse to send while a file is
    // still uploading so the user doesn't see their message fire without its
    // attachments.
    const pending = useAttachmentsStore.getState().selectForChannel(activeChannelId);
    const stillUploading = pending.some((a) => a.status === "uploading");
    if (stillUploading) {
      setSendError("Wait for uploads to finish.");
      return;
    }
    const readyIds = pending
      .filter((a) => a.status === "ready" && a.attachmentId)
      .map((a) => a.attachmentId as number);
    if (!value.trim() && readyIds.length === 0) return;

    setSending(true);
    setSendError(null);
    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: value.trim(),
        attachmentIds: readyIds,
      });
      editorRef.current?.clear();
      setInput("");
      useDraftsStore.getState().clearChannelDraft(activeChannelId);
      setPickerOpen(false);
      useAttachmentsStore.getState().clearChannel(activeChannelId);
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  // --- File picker + upload kick-off -------------------------------------

  const serverAttachmentConfig = useChatStore((s) => s.serverAttachmentConfig);

  const handleAttach = async () => {
    if (!activeServerId || !activeChannelId) return;
    const cfg = serverAttachmentConfig[activeServerId];
    if (!cfg || cfg.port === 0) {
      setSendError("This server does not support attachments.");
      return;
    }
    let picked: string[] | null = null;
    try {
      const selection = await openDialog({
        multiple: true,
        directory: false,
        title: "Choose files to attach",
      });
      if (!selection) return; // user cancelled
      picked = Array.isArray(selection) ? selection : [selection];
    } catch (err) {
      setSendError(`File picker failed: ${err}`);
      return;
    }
    if (!picked || picked.length === 0) return;

    for (const filePath of picked) {
      await startUpload(filePath);
    }
  };

  const startUpload = async (filePath: string) => {
    if (!activeServerId || !activeChannelId) return;
    // stat_attachment_file also decodes image dimensions for image MIME
    // types (0/0 for anything else) so we can feed them to the upload and
    // let the server broadcast them downstream.
    type StatResult = {
      filename: string;
      sizeBytes: number;
      mime: string;
      width: number;
      height: number;
    };
    let meta: StatResult;
    try {
      meta = await invoke<StatResult>("stat_attachment_file", { path: filePath });
    } catch (err) {
      setSendError(`Could not read ${filePath}: ${err}`);
      return;
    }

    const cfg = serverAttachmentConfig[activeServerId];
    if (cfg && cfg.maxBytes > 0 && meta.sizeBytes > cfg.maxBytes) {
      setSendError(
        `${meta.filename} is ${formatBytes(meta.sizeBytes)}; this server's cap is ${formatBytes(cfg.maxBytes)}.`
      );
      return;
    }

    const pendingId = `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    const entry: PendingAttachment = {
      pendingId,
      channelId: activeChannelId,
      filename: meta.filename,
      mime: meta.mime,
      kind: kindFromMime(meta.mime),
      totalBytes: meta.sizeBytes,
      transferredBytes: 0,
      status: "uploading",
    };
    useAttachmentsStore.getState().addPending(entry);

    // Fire-and-forget — upload progress/completion events update the store.
    invoke("upload_attachment", {
      req: {
        pendingId,
        serverId: activeServerId,
        channelId: activeChannelId,
        filePath,
        filename: meta.filename,
        mime: meta.mime,
        width: meta.width,
        height: meta.height,
      },
    }).catch((err) => {
      // The Rust path also emits attachment_upload_failed; this catch is
      // defensive so an exception from the bridge itself doesn't leave the
      // card spinning forever.
      useAttachmentsStore.getState().markFailed(pendingId, String(err), false);
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
        className="flex-1 overflow-y-auto"
      >
        {/* Inner content wrapper — what the ResizeObserver watches so any
            async height growth (image loads, etc.) re-pins to bottom. */}
        <div ref={messagesContentRef} className="px-4 py-4">
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
        </div>
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Pending attachments (uploaded / uploading) for this channel */}
      {activeChannelId && <PendingAttachmentsRow channelId={activeChannelId} />}

      {/* Input bar */}
      <div className="px-3 pb-2">
        <div className="flex min-h-[54px] items-center gap-2.5 rounded-xl border border-border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
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
