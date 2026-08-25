import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { useVirtuosoPrepend } from "./useVirtuosoPrepend";
import { prefetchAround } from "./attachmentPrefetch";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { channelKey } from "../../lib/channelKey";
import { toast } from "../../stores/toastStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useTypeToFocusComposer } from "./useTypeToFocusComposer";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import MessagePreview from "./MessagePreview";
import RichComposer from "./RichComposer";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import { pickFiles } from "./filePicker";
import { queueUpload, startQueuedUpload } from "./uploadAttachment";
import { chunkSourceFromPath } from "./chunkSource";
import WelcomeState from "./WelcomeState";
import DeleteMessageConfirmModal from "../../components/DeleteMessageConfirmModal";
import { useCanDeleteOthers } from "../servers/useCanDeleteOthers";
import { PERM, useChannelPermission } from "../servers/permissions";

function formatRemaining(totalSec: number): string {
  const s = Math.max(0, Math.floor(totalSec));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.ceil(s / 60)}m`;
  if (s < 86400) return `${Math.ceil(s / 3600)}h`;
  return `${Math.ceil(s / 86400)}d`;
}
import type { Message } from "../../types";

// How close (in messages) the rendered window's top edge may get to the
// oldest loaded message before the next history page is requested.
// Half a page: deep enough that the page-boundary group-flip (see
// maybeLoadOlderHistory) resolves off-screen at normal scroll speeds,
// shallow enough that idling at the bottom never pages.
const HISTORY_EAGER_THRESHOLD = 25;

// How many rows below the viewport's bottom edge before the "jump to
// present" pill appears for a plain scroll-up (no jump window involved).
const JUMP_PILL_ROWS = 20;

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
  const activeModal = useUiStore((s) => s.activeModal);
  const openModal = useUiStore((s) => s.openModal);
  const canDeleteOthers = useCanDeleteOthers(activeServerId);
  // Permissions v2: gate the composer on the server-resolved per-channel
  // bits (read-only channels, muted members). Server stays authoritative.
  const canSend = useChannelPermission(activeServerId, activeChannelId, PERM.SEND_MESSAGES);
  const canAttach = useChannelPermission(activeServerId, activeChannelId, PERM.ATTACH_FILES);
  const canBypassSlowmode = useChannelPermission(activeServerId, activeChannelId, PERM.MANAGE_MESSAGES);
  // Timed out (MODERATE_MEMBERS): the server rejects sends; show why.
  const ownUsername = useAuthStore((s) => s.username);
  const timedOutUntil = useChatStore((s) =>
    activeServerId
      ? s.membersByServer[activeServerId]?.find((m) => m.username === ownUsername)?.timedOutUntil ?? 0
      : 0,
  );
  const [nowSec, setNowSec] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    if (!timedOutUntil || timedOutUntil <= nowSec) return;
    const t = setInterval(() => setNowSec(Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(t);
  }, [timedOutUntil, nowSec]);
  const timedOut = timedOutUntil > nowSec;

  // Local state for which message is being deleted; the modal reads
  // this when it confirms. Tracked locally rather than in uiStore so
  // the modal lifecycle aligns with this panel's existence.
  const [pendingDeleteTarget, setPendingDeleteTarget] =
    useState<Message | null>(null);

  const [draft, setDraft] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  // A just-applied jump context window. `epoch` bumps on each jump so the
  // Virtuoso instance remounts and lands centered on `targetId` via
  // initialTopMostItemIndex — a fresh mount has no stale scroll anchor, so
  // the target is on screen the first time (the old scrollToIndex-after-
  // replace path jumped to a stale anchor first, then corrected). null when
  // viewing normally / at present.
  const [jumpWindow, setJumpWindow] = useState<{ epoch: number; targetId: number } | null>(null);
  // Travel direction of the last jump — picks the arrival slide's keyframe.
  const [jumpDir, setJumpDir] = useState<"up" | "down">("up");
  // True while the user has scrolled more than JUMP_PILL_ROWS above the live
  // bottom — shows the jump-to-present pill even without a jump window.
  const [scrolledUp, setScrolledUp] = useState(false);
  const editorRef = useRef<RichInputHandle>(null);
  // pendingIds already claimed by an in-flight send, so a second Enter
  // pressed while uploads are still running can't re-send the same
  // attachments as a duplicate message.
  const claimedPendingsRef = useRef<Set<string>>(new Set());
  // Single-flight guard for scroll-up history pagination.
  const loadMoreInFlightRef = useRef(false);
  // The beforeId of the last eager page request. The response arrives
  // via the channel_history_received event, not the invoke promise, so
  // the in-flight guard alone can't stop the threshold trigger from
  // re-requesting the same page in the window between invoke resolving
  // and the data landing.
  const lastRequestedBeforeIdRef = useRef(0);
  // Downward-pagination twins of the older-history guards above, for
  // fetching NEWER messages (after_id) around a jump target.
  const loadNewerInFlightRef = useRef(false);
  const lastRequestedAfterIdRef = useRef(0);
  // When a jump target isn't in the loaded slice, we request an around-window
  // and stash the target here; the effect that watches `messages` lands on it
  // once the window arrives.
  const pendingJumpIdRef = useRef<number | null>(null);
  // For ~1s after a jump remount, totalListHeightChanged re-pins the target
  // at the top. At mount, rows BELOW the target are still height-estimates;
  // when they under-estimate (code blocks), Virtuoso thinks the content
  // below is less than a viewport and clamps the scroll upward instead of
  // honoring initialTopMostItemIndex — then the rows measure tall and the
  // clamped anchor sticks, leaving the target mis-placed. Re-asserting on
  // each height change settles to exact as real heights land.
  const jumpAssertUntilRef = useRef(0);
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);
  const chatViewRef = useRef<HTMLDivElement>(null);

  // Stable per-message identity, shared by computeItemKey and the
  // prepend tracker so the two agree on what "the same message" is.
  // Optimistic bubbles have id 0 and carry a client nonce instead.
  const messageKey = useCallback(
    (m: { id: number; nonce?: string }) => (m.id > 0 ? m.id : m.nonce ?? ""),
    [],
  );

  // Track the chat viewport size and publish it to the store so
  // AttachmentList can scale image/video previews proportionally to
  // the available space (sqrt-based, see attachmentSizing.ts). On
  // unmount we clear the size so the helpers fall back to fixed
  // defaults instead of using a stale dimension.
  useLayoutEffect(() => {
    const el = chatViewRef.current;
    if (!el) return;
    const setSize = useChatStore.getState().setChatViewSize;
    // Round before publishing: the observer fires on sub-pixel
    // changes while a window is dragged, and every attachment on
    // screen recomputes its box off this value.
    const publish = (w: number, h: number) =>
      setSize({ width: Math.round(w), height: Math.round(h) });
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (!rect) return;
      publish(rect.width, rect.height);
    });
    observer.observe(el);
    // Seed an initial value before the first observer fire — useful
    // for the synchronous first render of attachments below.
    const rect = el.getBoundingClientRect();
    publish(rect.width, rect.height);
    return () => {
      observer.disconnect();
      setSize(null);
    };
  }, []);

  const channels = activeServerId ? channelsByServer[activeServerId] ?? [] : [];
  const channel = channels.find((c) => c.id === activeChannelId) ?? null;
  const channelName = channel?.name ?? activeChannelId ?? null;
  // Composite cache key — per-channel maps are namespaced by server
  // (bare channel ids collide: every server has a "general").
  const activeKey =
    activeServerId && activeChannelId
      ? channelKey(activeServerId, activeChannelId)
      : null;
  const messages = activeKey ? messagesByChannel[activeKey] ?? [] : [];
  const loading = activeKey ? historyLoading[activeKey] === true : false;
  // "Windowed" = viewing a jumped slice with newer messages below (not at the
  // live bottom). Drives the jump-to-present pill, disables followOutput's
  // auto-scroll, and enables downward pagination via endReached.
  const hasMoreAfterMap = useChatStore((s) => s.hasMoreAfter);
  const windowed = activeKey ? hasMoreAfterMap[activeKey] === true : false;
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

  // Keeps the viewport anchored when older history pages in at the top. The
  // reset key folds in the jump epoch so a jump-window remount also resets the
  // prepend offset (matching the Virtuoso key= below).
  const prependResetKey = activeKey ? `${activeKey}:${jumpWindow?.epoch ?? 0}` : null;
  const firstItemIndex = useVirtuosoPrepend(messages, messageKey, prependResetKey);

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
    // A just-applied jump window: mount with the target as the topmost
    // visible row. NOT align:"center" — centering makes Virtuoso walk up
    // from the target, filling half a viewport with ESTIMATED row heights to
    // pick the actual topmost row, and anchor that; when the surrounding
    // rows are code blocks (far taller than the estimate) the target gets
    // pushed well below center once they measure. Anchoring the target's own
    // top edge involves no estimates, so it cannot drift — and "jumped
    // message near the top + flash" is also how Discord lands.
    if (jumpWindow) {
      const pos = messages.findIndex((m) => m.id === jumpWindow.targetId);
      if (pos >= 0) return { index: pos, align: "start" as const };
    }
    if (!activeKey) return last;
    const saved =
      useChatStore.getState().scrollPositionsByChannel[activeKey];
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
    const serverId = activeServerId;
    const channelId = activeChannelId;
    return () => {
      if (serverId && channelId) {
        useChatStore
          .getState()
          .setScrollPosition(
            serverId,
            channelId,
            topIndexRef.current,
            atBottomRef.current,
          );
      }
    };
  }, [activeServerId, activeChannelId]);

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
    const serverId = activeServerId;
    const chId = activeChannelId;
    const key = channelKey(serverId, chId);
    const chat = useChatStore.getState();
    if (chat.historyFetched[key] || chat.historyLoading[key]) {
      return;
    }
    chat.setHistoryLoading(serverId, chId, true);
    invoke("request_channel_history", {
      serverId,
      channelId: chId,
      beforeId: 0,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history:", err);
      useChatStore.getState().setHistoryLoading(serverId, chId, false);
    });
  }, [activeServerId, activeChannelId]);

  // Restore the per-channel draft on channel switch (mirror the DM
  // pattern). The editor is uncontrolled, so without this it keeps the
  // previous channel's text — which could then be sent to the wrong
  // channel.
  useEffect(() => {
    const stored =
      activeServerId && activeChannelId
        ? useDraftsStore.getState().getChannelDraft(activeServerId, activeChannelId)
        : "";
    editorRef.current?.setValue(stored);
    setDraft(stored);
  }, [activeServerId, activeChannelId]);

  // Scroll-up paginator. Two triggers share this: Virtuoso's
  // startReached (the hard edge, `force` — keeps its retry semantics
  // if a response is ever lost) and an eager threshold off rangeChanged
  // (`!force`, deduped per page boundary via lastRequestedBeforeIdRef).
  //
  // The eager trigger exists because of a measured glitch (row-audit,
  // 2026-08-15): the oldest loaded message renders ungrouped — it has
  // no known predecessor — and when the next page prepends, the same
  // sender usually continues above it, so it re-renders grouped and
  // shrinks by the ~23px header. Paging at the startReached edge put
  // that resize exactly at the viewport edge the user was looking at;
  // paging ~25 messages early puts it on a row that is unmounted or
  // far off-screen, where Virtuoso's anchoring absorbs it invisibly.
  const maybeLoadOlderHistory = (force: boolean) => {
    if (!activeServerId || !activeChannelId || !activeKey) return;
    const chat = useChatStore.getState();
    if (!chat.hasMoreHistory[activeKey]) return;
    if (loadMoreInFlightRef.current) return;
    // messages are sorted by id ascending, so the first real-id entry is
    // the oldest loaded message.
    const list = chat.messagesByChannel[activeKey] ?? [];
    const oldest = list.find((m: { id: number }) => m.id > 0);
    const beforeId = oldest?.id ?? 0;
    if (!force && beforeId !== 0 && beforeId === lastRequestedBeforeIdRef.current) {
      return;
    }
    loadMoreInFlightRef.current = true;
    lastRequestedBeforeIdRef.current = beforeId;
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId,
      limit: 50,
    })
      .catch((err) => {
        console.error("request_channel_history (more):", err);
        // Allow the eager path to retry this boundary after a failure.
        lastRequestedBeforeIdRef.current = 0;
      })
      .finally(() => {
        loadMoreInFlightRef.current = false;
      });
  };

  // Downward paginator — the mirror of maybeLoadOlderHistory, active only
  // while windowed (newer messages exist below the jump slice). Virtuoso's
  // endReached fires it; it no-ops at present (hasMoreAfter false), so it's
  // harmless to leave wired in the non-jumped case.
  const maybeLoadNewerHistory = () => {
    if (!activeServerId || !activeChannelId || !activeKey) return;
    const chat = useChatStore.getState();
    if (!chat.hasMoreAfter[activeKey]) return;
    if (loadNewerInFlightRef.current) return;
    const list = chat.messagesByChannel[activeKey] ?? [];
    let afterId = 0;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i].id > 0) {
        afterId = list[i].id;
        break;
      }
    }
    if (afterId === 0 || afterId === lastRequestedAfterIdRef.current) return;
    loadNewerInFlightRef.current = true;
    lastRequestedAfterIdRef.current = afterId;
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: 0,
      afterId,
      limit: 50,
    })
      .catch((err) => {
        console.error("request_channel_history (newer):", err);
        lastRequestedAfterIdRef.current = 0;
      })
      .finally(() => {
        loadNewerInFlightRef.current = false;
      });
  };

  // "Jump to present": drop the windowed slice and reload the newest page,
  // exactly like a fresh channel entry. Clearing first (rather than merging)
  // guarantees no gap between the old window and the tail, and flips
  // hasMoreAfter back to false so followOutput/the pill re-sync.
  const jumpToPresent = () => {
    if (!activeServerId || !activeChannelId) return;
    const serverId = activeServerId;
    const channelId = activeChannelId;
    setJumpWindow(null);
    pendingJumpIdRef.current = null;
    loadMoreInFlightRef.current = false;
    lastRequestedBeforeIdRef.current = 0;
    loadNewerInFlightRef.current = false;
    lastRequestedAfterIdRef.current = 0;
    const chat = useChatStore.getState();
    chat.resetChannelForJump(serverId, channelId);
    chat.setHistoryLoading(serverId, channelId, true);
    invoke("request_channel_history", {
      serverId,
      channelId,
      beforeId: 0,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history (present):", err);
      useChatStore.getState().setHistoryLoading(serverId, channelId, false);
    });
  };


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
    // No filters — Windows remembers the last-used type filter per
    // app session, so passing a list with "Images" or similar in it
    // means the dialog reopens with that selection. We want "All
    // Files" every time so attachment sending isn't a 2-click flow.
    const paths = await pickFiles({ multiple: true });
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

    // Sending while viewing a jumped slice would drop the new message past a
    // hidden gap (addMessage guards against it). Snap to present first and
    // keep the draft — the next Enter sends normally at the bottom.
    if (activeKey && useChatStore.getState().hasMoreAfter[activeKey]) {
      jumpToPresent();
      return;
    }

    const channelPendings = useAttachmentsStore
      .getState()
      .selectForChannel(activeServerId, activeChannelId);

    // Only claim pendings that are still live AND not already claimed by
    // an in-flight send. Without the claimed guard, a second Enter pressed
    // while uploads are running re-reads the SAME live pendings and fires a
    // duplicate message referencing the same attachments.
    const livePendings = channelPendings.filter(
      (p) =>
        p.status !== "failed" && !claimedPendingsRef.current.has(p.pendingId),
    );
    const pendingIds = livePendings.map((p) => p.pendingId);
    if (!content && pendingIds.length === 0) return;
    for (const id of pendingIds) claimedPendingsRef.current.add(id);
    const releaseClaims = () => {
      for (const id of pendingIds) claimedPendingsRef.current.delete(id);
    };

    // Capture the target channel: activeChannelId can change while we
    // await uploads / the send, and this message belongs to the channel
    // it was composed in.
    const serverId = activeServerId;
    const channelId = activeChannelId;

    const replyToId = replyingTo?.id && replyingTo.id > 0 ? replyingTo.id : undefined;
    const nonce = generateNonce();
    useChatStore.getState().addMessage(serverId, {
      id: 0,
      channelId,
      sender: username,
      content,
      timestamp: String(Math.floor(Date.now() / 1000)),
      attachments: [],
      nonce,
      pendingAttachmentIds: pendingIds.length > 0 ? pendingIds : undefined,
      replyTo: replyToId,
    });
    editorRef.current?.clear();
    setDraft("");
    setReplyingTo(null);
    useDraftsStore.getState().clearChannelDraft(serverId, channelId);

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

    // Nothing left to send: every attachment failed and there's no text.
    // Drop the optimistic bubble and surface the failure rather than
    // firing an empty message at the server.
    if (!content && attachmentIds.length === 0) {
      useChatStore.getState().removeMessageByNonce(serverId, channelId, nonce);
      if (pendingIds.length > 0) toast.error("Attachment upload failed");
      for (const id of pendingIds) {
        useAttachmentsStore.getState().removePending(id);
      }
      releaseClaims();
      return;
    }

    try {
      await invoke("send_channel_message", {
        serverId,
        channelId,
        message: content,
        attachmentIds: attachmentIds.length > 0 ? attachmentIds : undefined,
        nonce,
        replyTo: replyToId,
      });
      // Message went out (with whatever uploaded). Surface any partial
      // upload failures instead of dropping their chips silently.
      const failed = pendingIds.filter(
        (id) =>
          useAttachmentsStore.getState().pendings[id]?.status === "failed",
      ).length;
      if (failed > 0) {
        toast.error(
          `${failed} attachment${failed > 1 ? "s" : ""} failed to upload`,
        );
      }
      for (const id of pendingIds) {
        useAttachmentsStore.getState().removePending(id);
      }
    } catch (err) {
      console.error("send_channel_message:", err);
      // Never delivered — drop the phantom "sent" bubble and let the user
      // retry with their text restored (queued attachments stay for the
      // retry). Previously this left an undeletable id-0 bubble forever.
      useChatStore.getState().removeMessageByNonce(serverId, channelId, nonce);
      toast.error("Failed to send message");
      if (content) {
        setDraft(content);
        editorRef.current?.setValue(content);
        useDraftsStore.getState().setChannelDraft(serverId, channelId, content);
      }
    } finally {
      releaseClaims();
    }
  };

  // Append a composer-built snippet (```lang fence / $$math$$) to the
  // draft. setValue drives RichInput's onChange, so the draft state,
  // drafts store, and the live send-preview all update through the
  // normal pipeline.
  const insertSnippet = useCallback((snippet: string) => {
    const cur = editorRef.current?.getValue() ?? "";
    const sep = cur.length > 0 && !cur.endsWith("\n") ? "\n" : "";
    editorRef.current?.focus();
    editorRef.current?.setValue(cur + sep + snippet);
  }, []);

  // Suppress the input wrapper's default drop-handling so dragged
  // files don't appear as a path string in the message input —
  // useDragDrop at the window level handles the upload.
  const suppressDrop = (e: React.DragEvent<HTMLElement>) => {
    e.preventDefault();
  };

  // Fire the delete flow for a channel message. Optimistic: snapshot
  // into pendingDeletions, remove from the view, fire the native
  // command, and start a 5-second watchdog. The watchdog only acts
  // if no `channel_message_delete_responded` event arrives by then —
  // useServerEvents handles success (clear pending) and failure
  // (restore via mergeMessage + toast.error).
  // useCallback on the delete pair keeps their identity stable across
  // renders. `onDelete` is a MessageBubble prop, and memo(MessageBubble)
  // compares props by reference — a fresh closure here re-rendered every
  // visible bubble on every panel render (each keystroke, each history
  // prepend), which is a long frame exactly when the user is scrolling.
  const handleDeleteChannelMessage = useCallback((message: Message) => {
    const { activeServerId, activeChannelId } = useChatStore.getState();
    if (!activeServerId || !activeChannelId || typeof message.id !== "number") return;
    const serverId = activeServerId;
    const channelId = activeChannelId;
    const messageId = message.id;

    useChatStore.getState().snapshotAndRemove(serverId, channelId, messageId);

    invoke("delete_channel_message", { serverId, channelId, messageId }).catch(
      (err) => {
        console.error("delete_channel_message:", err);
        useChatStore.getState().restorePendingDeletion(serverId, channelId, messageId);
        toast.error("Failed to delete message", "Please try again.");
      },
    );

    // 5-second watchdog: if no response/broadcast arrives by then,
    // restore the bubble (network probably hung).
    window.setTimeout(() => {
      const stillPending = useChatStore
        .getState()
        .pendingDeletions[channelKey(serverId, channelId)]?.has(messageId);
      if (stillPending) {
        useChatStore.getState().restorePendingDeletion(serverId, channelId, messageId);
        toast.error(
          "Delete timed out",
          "Couldn't reach the server. Please try again.",
        );
      }
    }, 5000);
  }, []);

  const requestDeleteChannelMessage = useCallback(
    (message: Message, options?: { skipConfirm?: boolean }) => {
      if (typeof message.id !== "number" || message.id <= 0) return;
      if (options?.skipConfirm) {
        // Shift+click: power-user path. Delete immediately, no modal.
        handleDeleteChannelMessage(message);
        return;
      }
      setPendingDeleteTarget(message);
      openModal("delete-message-confirm");
    },
    [handleDeleteChannelMessage, openModal],
  );

  // --- Message editing (own messages only) ---
  const [editingMessageId, setEditingMessageId] = useState<number | null>(null);
  const startEdit = useCallback((message: Message) => {
    if (typeof message.id === "number" && message.id > 0) setEditingMessageId(message.id);
  }, []);
  const cancelEdit = useCallback(() => setEditingMessageId(null), []);
  const submitEdit = useCallback((message: Message, content: string) => {
    const { activeServerId, activeChannelId } = useChatStore.getState();
    setEditingMessageId(null);
    if (!activeServerId || !activeChannelId || typeof message.id !== "number") return;
    if (content === message.content) return; // no change → skip round-trip
    invoke("edit_channel_message", {
      serverId: activeServerId,
      channelId: activeChannelId,
      messageId: message.id,
      content,
    }).catch((err) => console.error("edit_channel_message:", err));
  }, []);
  // ArrowUp on an empty composer → edit the latest own message in view.
  const editLatestOwn = useCallback(() => {
    const { activeServerId, activeChannelId, messagesByChannel: byCh } = useChatStore.getState();
    if (!activeServerId || !activeChannelId || !username) return;
    const key = channelKey(activeServerId, activeChannelId);
    const list = byCh[key] ?? [];
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i].sender === username && list[i].id > 0) {
        setEditingMessageId(list[i].id);
        return;
      }
    }
  }, [username]);

  // Type anywhere in the channel to start composing; ArrowUp edits the latest.
  useTypeToFocusComposer(editorRef, editLatestOwn);

  // --- Replies ---
  const [replyingTo, setReplyingTo] = useState<Message | null>(null);
  const startReply = useCallback((m: Message) => {
    if (typeof m.id === "number" && m.id > 0) {
      setReplyingTo(m);
      editorRef.current?.focus();
    }
  }, []);
  const cancelReply = useCallback(() => setReplyingTo(null), []);
  // Resolve reply previews: parent message by id, for the loaded page.
  const messagesById = useMemo(() => {
    const map = new Map<number, Message>();
    for (const m of messages) if (m.id > 0) map.set(m.id, m);
    return map;
  }, [messages]);
  // Reset transient composer + jump state when switching channels.
  useEffect(() => {
    setReplyingTo(null);
    setEditingMessageId(null);
    setJumpWindow(null);
    setScrolledUp(false);
    pendingJumpIdRef.current = null;
    lastRequestedAfterIdRef.current = 0;
    loadNewerInFlightRef.current = false;
  }, [activeKey]);

  // Jump to a replied-to message + flash it. If the parent is in the loaded
  // slice we scroll straight to it; otherwise (Discord-style) we fetch a
  // context window around it and let the landing effect below center on it
  // once it arrives — without pulling in all the intervening history.
  const [highlightId, setHighlightId] = useState<number | null>(null);
  const highlightTimer = useRef<number | null>(null);
  const flash = useCallback((id: number) => {
    if (highlightTimer.current) window.clearTimeout(highlightTimer.current);
    setHighlightId(id);
    highlightTimer.current = window.setTimeout(() => setHighlightId(null), 1600);
  }, []);
  // Identity-stable (reads the store at call time). This is a
  // memo(MessageBubble) prop: depending on `messages` gave it a fresh
  // identity on every prepend / live arrival, which re-rendered every
  // visible bubble exactly while the user was scrolling — the cause of the
  // attachment-scroll glitch regression.
  const jumpToMessage = useCallback(
    (id: number) => {
      const chat = useChatStore.getState();
      const serverId = chat.activeServerId;
      const channelId = chat.activeChannelId;
      if (!serverId || !channelId) return;
      const list = chat.messagesByChannel[channelKey(serverId, channelId)] ?? [];
      const pos = list.findIndex((m) => m.id === id);
      if (pos >= 0) {
        // Structural jump first, visual motion second: remount centered on
        // the target (exact — everything around it mounts measured), then a
        // short CSS slide on the fresh list conveys the travel direction
        // (jumpArriveUp/Down in globals.css). Animating the real scroll
        // position through a virtualized list cannot be made clean — rows
        // measuring mid-flight re-anchor the scroller under the animation
        // (native smooth, paused paging, settle-snap, and a live-rect rAF
        // loop all glitched) — while a transform slide never touches scroll
        // geometry, so it cannot mis-land.
        setJumpDir(pos < topIndexRef.current ? "up" : "down");
        jumpAssertUntilRef.current = Date.now() + 1000;
        setJumpWindow((prev) => ({ epoch: (prev?.epoch ?? 0) + 1, targetId: id }));
        flash(id);
        return;
      }
      // Not loaded — fetch a window centered on it. Already requesting this
      // exact target? Wait for the landing effect.
      if (pendingJumpIdRef.current === id) return;
      pendingJumpIdRef.current = id;
      loadMoreInFlightRef.current = false;
      lastRequestedBeforeIdRef.current = 0;
      loadNewerInFlightRef.current = false;
      lastRequestedAfterIdRef.current = 0;
      chat.setHistoryLoading(serverId, channelId, true);
      invoke("request_channel_history", {
        serverId,
        channelId,
        beforeId: 0,
        aroundId: id,
        limit: 25,
      }).catch((err) => {
        console.error("request_channel_history (around):", err);
        pendingJumpIdRef.current = null;
        useChatStore.getState().setHistoryLoading(serverId, channelId, false);
      });
    },
    [flash],
  );
  // Landing effect: once the requested around-window is in `messages`, remount
  // the list centered on the target (bump epoch) and flash it.
  useEffect(() => {
    const id = pendingJumpIdRef.current;
    if (id == null) return;
    if (!messages.some((m) => m.id === id)) return;
    pendingJumpIdRef.current = null;
    // An around-window target is (virtually) always older than what was
    // loaded — arrive with the upward-travel slide.
    setJumpDir("up");
    jumpAssertUntilRef.current = Date.now() + 1000;
    setJumpWindow((prev) => ({ epoch: (prev?.epoch ?? 0) + 1, targetId: id }));
    flash(id);
  }, [messages, flash]);
  useEffect(() => () => {
    if (highlightTimer.current) window.clearTimeout(highlightTimer.current);
  }, []);

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
      <div className="flex h-12 items-center border-b border-border-divider px-4 font-channel text-title font-emphasis tracking-title text-text-bright">
        <span className="mr-1.5 text-text-muted">#</span>
        {channelName}
      </div>

      <div ref={chatViewRef} className="relative flex flex-1 flex-col overflow-hidden">
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
            key={`${activeKey ?? "none"}:${jumpWindow?.epoch ?? 0}`}
            ref={virtuosoRef}
            data={messages}
            firstItemIndex={firstItemIndex}
            computeItemKey={(index, m) => messageKey(m) || `i:${index}`}
            initialTopMostItemIndex={initialIndex}
            // No auto-scroll-to-bottom while windowed: new live messages are
            // dropped (they'd land past a hidden gap), and downward paging
            // shouldn't yank the viewport to the freshly-appended tail.
            followOutput={windowed ? false : "smooth"}
            // Discord-style: when total content height < viewport,
            // stack messages from the bottom up against the input
            // bar instead of pinning the first message to the top.
            // Only affects the few-messages case; long histories are
            // unchanged.
            alignToBottom={true}
            // Mount rows well before they reach the edge, so anything
            // that settles after mount — an image decoding, a poster
            // frame landing — happens off-screen and the row scrolls in
            // already correct.
            increaseViewportBy={{ top: 600, bottom: 600 }}
            startReached={() => maybeLoadOlderHistory(true)}
            endReached={() => maybeLoadNewerHistory()}
            totalListHeightChanged={() => {
              // Landing assertion — see jumpAssertUntilRef.
              if (Date.now() > jumpAssertUntilRef.current || !jumpWindow) return;
              const p = messages.findIndex((m) => m.id === jumpWindow.targetId);
              if (p >= 0)
                virtuosoRef.current?.scrollToIndex({ index: p, align: "start" });
            }}
            rangeChanged={(range) => {
              // Absolute, like itemContent's — rebase before it is used
              // as a position in `messages`. Storing it raw meant the
              // saved scroll position was ~1e6 and the restore clamp
              // rejected it, so every channel switch landed at the
              // bottom.
              const start = range.startIndex - firstItemIndex;
              const end = range.endIndex - firstItemIndex;
              topIndexRef.current = start;
              // Far enough above the live bottom → surface the pill. React
              // bails out when the value is unchanged, so this is cheap.
              setScrolledUp(messages.length - 1 - end > JUMP_PILL_ROWS);
              // Warm the previews either side of the window so a row's
              // image is already cached by the time it mounts.
              prefetchAround(
                messages,
                start,
                end,
                activeServerId,
                useChatStore.getState().chatViewSize,
              );
              // Eager pagination: fetch the next page while the
              // boundary row is still far from the viewport, so its
              // group-flip resize (see maybeLoadOlderHistory) happens
              // off-screen. Small channels eagerly load exactly one
              // extra page on open, then the guard stops it.
              if (start < HISTORY_EAGER_THRESHOLD) maybeLoadOlderHistory(false);
            }}
            atBottomStateChange={(atBottom) => {
              atBottomRef.current = atBottom;
              if (atBottom) setScrolledUp(false);
            }}
            itemContent={(index, message) => {
              // firstItemIndex makes `index` absolute (it starts at
              // firstItemIndex, not 0), so it has to be rebased before
              // it can index `messages`. Without this every lookup was
              // out of bounds: grouping saw no previous message and
              // nothing ever grouped.
              const i = index - firstItemIndex;
              return (
              <MessageBubble
                message={message}
                grouped={
                  shouldGroup(i > 0 ? messages[i - 1] : undefined, message) &&
                  !message.replyTo
                }
                serverId={activeServerId}
                isLast={i === messages.length - 1}
                // Align avatar's left edge with the input bar card's
                // left edge: outer wrapper `px-3` = 12px from chat
                // panel's left. The card's rounded border starts there.
                paddingLeft={12}
                canDelete={
                  typeof message.id === "number" &&
                  message.id > 0 &&
                  (message.sender === username || canDeleteOthers)
                }
                onDelete={requestDeleteChannelMessage}
                canEdit={
                  typeof message.id === "number" &&
                  message.id > 0 &&
                  message.sender === username
                }
                editing={editingMessageId === message.id && message.id > 0}
                onStartEdit={startEdit}
                onSubmitEdit={submitEdit}
                onCancelEdit={cancelEdit}
                canReply={typeof message.id === "number" && message.id > 0}
                onReply={startReply}
                replyToSender={
                  message.replyTo
                    ? messagesById.get(message.replyTo)?.sender ??
                      (message.replyToSender || undefined)
                    : undefined
                }
                replyToContent={
                  message.replyTo
                    ? messagesById.get(message.replyTo)?.content ??
                      (message.replyToContent || undefined)
                    : undefined
                }
                onJumpToReply={jumpToMessage}
                highlighted={highlightId === message.id && message.id > 0}
              />
              );
            }}
            // Arrival slide replays per jump: the epoch in key= makes each
            // jump a fresh element, so the mount animation runs again.
            className={`flex-1 ${
              jumpWindow
                ? jumpDir === "up"
                  ? "animate-[jumpArriveUp_0.26s_ease-out_both]"
                  : "animate-[jumpArriveDown_0.26s_ease-out_both]"
                : ""
            }`}
          />
        )}

        {/* Jump-to-present pill — shown while viewing a jumped slice (newer
            messages hidden below → reloads the newest page) and while merely
            scrolled well above the live bottom (→ scrolls back down). Styled
            like the client's accent buttons; rounded-md tracks the theme's
            radius scale (flat on console, soft on default). */}
        {(windowed || scrolledUp) && (
          <button
            onClick={() => {
              if (windowed) {
                jumpToPresent();
                return;
              }
              // History below is contiguous — just go to the bottom.
              virtuosoRef.current?.scrollToIndex({ index: "LAST", align: "end" });
            }}
            title="Jump to present"
            className="absolute bottom-3 right-4 z-20 flex animate-[fadeUp_0.18s_ease_both] items-center gap-1.5 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent shadow-float transition-colors hover:bg-accent-hover"
          >
            Jump to present
            <svg
              className="h-3.5 w-3.5"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <polyline points="7 6 12 11 17 6" />
              <polyline points="7 13 12 18 17 13" />
            </svg>
          </button>
        )}
      </div>

      {/* Input bar — pending attachments live INSIDE the rounded chrome
          so adding files visibly expands the bar upward (Discord pattern).
          Drop-target wiring: the bar lights up while a drag is in flight,
          saturated state when the cursor is over the bar. */}
      {!canSend || timedOut ? (
        <div className="px-3 py-2">
          <div className="flex min-h-[54px] items-center rounded-lg border border-border bg-bg-light px-3.5 py-2.5 text-[13px] text-text-muted">
            {timedOut
              ? `You are timed out. You can send messages again in ${formatRemaining(timedOutUntil - nowSec)}.`
              : `You don't have permission to send messages in #${channelName ?? "channel"}.`}
          </div>
        </div>
      ) : (
      <div className="px-3 py-2" data-drop-target="active-input">
        <div
          onDragOver={suppressDrop}
          onDrop={suppressDrop}
          className={`relative flex min-h-[54px] flex-col gap-2.5 rounded-lg border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-ring ${
            dropHoveredHere
              ? "border-accent bg-accent-soft/50 animate-[dropTargetIn_0.18s_ease_both]"
              : dragActive
                ? "border-transparent animate-[dropPulse_1.6s_ease-in-out_infinite]"
                : "border-border"
          }`}
        >
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
          <MessagePreview draft={draft} />
          <PendingAttachmentsRow />
          <div className="flex items-center gap-2.5">
            {dropHoveredHere && (
              <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-lg bg-gradient-to-b from-accent-soft/80 to-accent-soft/40 backdrop-blur-[3px]">
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
            {canAttach && (
            <button
              onClick={handlePickFiles}
              title="Attach files"
              className="flex h-[34px] w-[34px] shrink-0 self-end items-center justify-center rounded-md bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="12" y1="8" x2="12" y2="16" />
                <line x1="8" y1="12" x2="16" y2="12" />
              </svg>
            </button>
            )}
            <RichInput
              ref={editorRef}
              onChange={(value) => {
                setDraft(value);
                if (activeServerId && activeChannelId)
                  useDraftsStore
                    .getState()
                    .setChannelDraft(activeServerId, activeChannelId, value);
              }}
              onEnter={handleSend}
              onArrowUpEmpty={editLatestOwn}
              placeholder={
                channel?.slowmodeSeconds && !canBypassSlowmode
                  ? `Message #${channelName ?? "channel"} · slowmode ${formatRemaining(channel.slowmodeSeconds)}`
                  : `Message #${channelName ?? "channel"}`
              }
              maxHeight={160}
              className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            />
            <div className="flex shrink-0 self-end gap-1">
              <RichComposer onInsert={insertSnippet} />
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
                className="flex h-[34px] w-[34px] shrink-0 items-center justify-center rounded-md bg-accent text-on-accent transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
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
      )}

      {activeModal === "delete-message-confirm" && pendingDeleteTarget && (
        <DeleteMessageConfirmModal
          onConfirm={() => {
            handleDeleteChannelMessage(pendingDeleteTarget);
            setPendingDeleteTarget(null);
          }}
        />
      )}
    </div>
  );
}
