import { useState, useRef, useEffect, useMemo, memo } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import VoiceParticipantList from "../voice/VoiceParticipantList";
import ServerActionsDropdown from "../servers/ServerActionsDropdown";
import ServerSettingsModal from "../servers/ServerSettingsModal";
import LeaveServerConfirmModal from "../../components/LeaveServerConfirmModal";
import { PERM, usePermission } from "../servers/permissions";
import CreateChannelModal from "./CreateChannelModal";
import { joinVoiceChannel } from "../voice/streaming/joinVoiceChannel";
import { useSidebarResize } from "./useSidebarResize";
import type { ChannelInfo } from "../../types";

/// Server-mode sidebar. Mounted when activeView is "server" or
/// "voice". Subscribes only to server/channel/voice slices — DM
/// conversations and friends-list updates don't reach this component,
/// so a new DM doesn't trigger a re-render of the channel list.
export default function ServerChannelsSidebar() {
  const { wrapperRef, width, onResizeMouseDown } = useSidebarResize();

  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const servers = useChatStore((s) => s.servers);
  const serverMeta = useChatStore((s) => s.serverMeta);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const channelPresence = useVoiceStore((s) => s.channelPresence);
  const setActiveView = useUiStore((s) => s.setActiveView);
  // Drag/drop state lives inside TextChannelRow now — each row owns
  // its own per-row dragHoveredKey and dragActive subscriptions so
  // parent re-renders don't ripple through every channel.

  const [collapsedCategories, setCollapsedCategories] = useState<Set<string>>(
    new Set(),
  );
  const [createTarget, setCreateTarget] = useState<{
    type: "text" | "voice" | "category";
    categoryId?: string;
    allowTypeChoice?: boolean;
  } | null>(null);
  /// Context menu over the channel list: a channel/category row, or the
  /// empty area (create actions). Only shown to MANAGE_CHANNELS holders.
  const [ctxMenu, setCtxMenu] = useState<{
    x: number;
    y: number;
    target:
      | { kind: "channel" | "category"; id: string; name: string }
      | { kind: "empty" };
  } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{
    id: string;
    name: string;
    isCategory: boolean;
  } | null>(null);
  /// Drag-reorder state: the id being dragged and the insertion point
  /// ("insert before item X", or END for the very bottom).
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropBefore, setDropBefore] = useState<string | "END" | null>(null);
  const [showServerMenu, setShowServerMenu] = useState(false);
  const [leavePending, setLeavePending] = useState<{ id: string; name: string } | null>(null);
  const serverMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (
        serverMenuRef.current &&
        !serverMenuRef.current.contains(e.target as Node)
      ) {
        setShowServerMenu(false);
      }
    };
    if (showServerMenu) document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showServerMenu]);

  // Channel-list inputs change rarely (server switch / channel CRUD).
  // Memo avoids the repeated O(N) filter walks on every drag-state
  // toggle and every voice presence update.
  const channels = useMemo(
    () => (activeServerId ? channelsByServer[activeServerId] ?? [] : []),
    [activeServerId, channelsByServer],
  );
  // Flat Discord-style grouping: a channel belongs to the nearest
  // category ABOVE it in the ordered list; channels before the first
  // category are uncategorized and render at the top with no header.
  const grouped = useMemo(() => {
    const lead: ChannelInfo[] = [];
    const blocks: { category: ChannelInfo; children: ChannelInfo[] }[] = [];
    for (const ch of channels) {
      if (ch.type === "category") blocks.push({ category: ch, children: [] });
      else if (blocks.length === 0) lead.push(ch);
      else blocks[blocks.length - 1].children.push(ch);
    }
    return { lead, blocks };
  }, [channels]);
  const orderedIds = useMemo(() => channels.map((c) => c.id), [channels]);
  const serverName = useMemo(
    () =>
      (activeServerId ? serverMeta[activeServerId]?.name : undefined) ??
      servers.find((s) => s.id === activeServerId)?.name,
    [activeServerId, serverMeta, servers],
  );
  const canInvite = usePermission(activeServerId, PERM.MANAGE_INVITES);
  const canManageChannels = usePermission(activeServerId, PERM.MANAGE_CHANNELS);
  const activeModal = useUiStore((s) => s.activeModal);

  // Close the channel-list context menu on any outside click / Esc.
  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [ctxMenu]);

  // ── Drag-reorder (channels + categories, one flat list) ─────────
  //
  // HTML5 DnD, delegated at the list container off data-reorder-id
  // attributes so the memoized rows don't grow handler props. The
  // file-drop system is unaffected: it only reacts to drags whose
  // dataTransfer carries Files, which ours never do.

  const applyReorder = (draggedId: string, before: string | "END") => {
    if (!activeServerId) return;
    const ids = [...orderedIds];
    const start = ids.indexOf(draggedId);
    if (start === -1) return;
    // A category moves with its whole block of children.
    let len = 1;
    if (channels[start]?.type === "category") {
      for (let i = start + 1; i < channels.length; i++) {
        if (channels[i].type === "category") break;
        len++;
      }
    }
    const block = ids.splice(start, len);
    const target = before === "END" ? ids.length : ids.indexOf(before);
    // `before` inside the dragged block (e.g. a category dropped onto
    // itself) → no-op.
    if (target === -1) return;
    ids.splice(target, 0, ...block);
    if (ids.every((id, i) => id === orderedIds[i])) return;
    // Optimistic: reorder locally now; the server's broadcast confirms
    // (or resyncs on rejection).
    const byId = new Map(channels.map((c) => [c.id, c]));
    const next = ids
      .map((id) => byId.get(id))
      .filter((c): c is ChannelInfo => !!c);
    useChatStore.getState().setChannelsForServer(activeServerId, next);
    invoke("reorder_channels", {
      serverId: activeServerId,
      channelIds: ids,
    }).catch(console.error);
  };

  const onListDragStart = (e: React.DragEvent) => {
    const el = (e.target as HTMLElement).closest(
      "[data-reorder-id]",
    ) as HTMLElement | null;
    if (!el || !canManageChannels) {
      e.preventDefault();
      return;
    }
    const id = el.dataset.reorderId!;
    e.dataTransfer.setData("application/x-decibell-reorder", id);
    e.dataTransfer.effectAllowed = "move";
    setDragId(id);
  };

  const onListDragOver = (e: React.DragEvent) => {
    if (!dragId) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    const el = (e.target as HTMLElement).closest(
      "[data-reorder-id]",
    ) as HTMLElement | null;
    let before: string | "END" = "END";
    if (el) {
      const id = el.dataset.reorderId!;
      const rect = el.getBoundingClientRect();
      if (e.clientY < rect.top + rect.height / 2) {
        before = id;
      } else {
        const idx = orderedIds.indexOf(id);
        before =
          idx >= 0 && idx + 1 < orderedIds.length
            ? orderedIds[idx + 1]
            : "END";
      }
    }
    // Categories can't nest: snap a category's drop point forward to
    // the next block boundary so it never lands inside another block
    // (and never captures the top uncategorized area).
    if (channels.find((c) => c.id === dragId)?.type === "category" && before !== "END") {
      const idx = orderedIds.indexOf(before);
      let snapped: string | "END" = "END";
      for (let i = idx; i >= 0 && i < channels.length; i++) {
        if (channels[i].type === "category") {
          snapped = channels[i].id;
          break;
        }
      }
      before = snapped;
    }
    setDropBefore(before);
  };

  const onListDrop = (e: React.DragEvent) => {
    if (!dragId) return;
    e.preventDefault();
    const id = dragId;
    const before = dropBefore;
    setDragId(null);
    setDropBefore(null);
    if (before) applyReorder(id, before);
  };

  const onListDragEnd = () => {
    setDragId(null);
    setDropBefore(null);
  };

  const onListContextMenu = (e: React.MouseEvent) => {
    if (!canManageChannels || !activeServerId) return;
    e.preventDefault();
    const el = (e.target as HTMLElement).closest(
      "[data-reorder-id]",
    ) as HTMLElement | null;
    if (el) {
      const id = el.dataset.reorderId!;
      const ch = channels.find((c) => c.id === id);
      if (!ch) return;
      setCtxMenu({
        x: e.clientX,
        y: e.clientY,
        target: {
          kind: ch.type === "category" ? "category" : "channel",
          id,
          name: ch.name,
        },
      });
    } else {
      setCtxMenu({ x: e.clientX, y: e.clientY, target: { kind: "empty" } });
    }
  };

  const toggleCategory = (id: string) => {
    setCollapsedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  // Text channel click handler lives inside TextChannelRow now (it
  // reads serverId/channelId from its own props + getState()).
  // History fetch is centralised in ChatPanel's effect.

  const handleVoiceChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId === connectedChannelId) {
      setActiveView("voice");
      return;
    }
    joinVoiceChannel(activeServerId, channelId).catch(console.error);
    setActiveView("voice");
  };

  return (
    <div
      ref={wrapperRef}
      className="relative flex shrink-0 flex-col border-r border-border bg-bg-dark pb-14"
      style={{ width }}
    >
      {/* Server name header */}
      <div
        className="relative flex h-12 shrink-0 items-center gap-2 border-b border-border px-4"
        ref={serverMenuRef}
      >
        <button
          onClick={() => activeServerId && setShowServerMenu((v) => !v)}
          disabled={!activeServerId}
          className="-ml-2 flex flex-1 cursor-pointer items-center gap-1.5 truncate rounded-md px-2 py-1.5 text-left transition-colors hover:bg-surface-hover disabled:cursor-default disabled:hover:bg-transparent"
          title={activeServerId ? "Server options" : undefined}
        >
          <span className="truncate font-display text-title font-emphasis tracking-title text-text-bright">
            {serverName ?? "Server"}
          </span>
          {activeServerId && (
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              className={`shrink-0 text-text-muted transition-transform ${
                showServerMenu ? "rotate-180" : ""
              }`}
            >
              <polyline points="6 9 12 15 18 9" />
            </svg>
          )}
        </button>
        {servers.some((s) => s.id === activeServerId) ? (
          <span className="rounded-sm bg-success/15 px-1.5 py-0.5 font-mono text-micro font-medium uppercase leading-none tracking-[0.1em] text-success">
            Public
          </span>
        ) : (
          <span className="rounded-sm bg-text-muted/15 px-1.5 py-0.5 font-mono text-micro font-medium uppercase leading-none tracking-[0.1em] text-text-secondary">
            Private
          </span>
        )}

        {showServerMenu && activeServerId && (
          <ServerActionsDropdown
            canInvite={canInvite}
            onInvites={() => {
              setShowServerMenu(false);
              useUiStore.getState().openModal("invite-manage");
            }}
            onServerSettings={() => {
              setShowServerMenu(false);
              useUiStore.getState().openModal("server-settings");
            }}
            onDisconnect={() => {
              setShowServerMenu(false);
              if (!activeServerId) return;
              const server = useChatStore
                .getState()
                .servers.find((s) => s.id === activeServerId);
              setLeavePending({
                id: activeServerId,
                name: server?.name ?? "this server",
              });
              useUiStore.getState().openModal("leave-server-confirm");
            }}
          />
        )}
      </div>

      {/* Channel list — one flat ordered list: uncategorized channels
          at the top, then category blocks. Rows carry data-reorder-id;
          drag/drop + context-menu handlers are delegated here. */}
      <div
        className="flex-1 overflow-y-auto px-2 py-2.5"
        style={{ "--list-row-pad-y": "7px", "--list-row-pad-x": "10px", "--list-row-gap": "8px" } as React.CSSProperties}
        onContextMenu={onListContextMenu}
        onDragStart={onListDragStart}
        onDragOver={onListDragOver}
        onDrop={onListDrop}
        onDragEnd={onListDragEnd}
      >
        {/* Uncategorized area (no header, always at the top) */}
        {activeServerId &&
          grouped.lead.map((ch) => (
            <div key={ch.id}>
              {dragId && dropBefore === ch.id && <DropLine />}
              {ch.type === "voice" ? (
                <VoiceRow
                  channel={ch}
                  presence={channelPresence[ch.id] ?? []}
                  connectedChannelId={connectedChannelId}
                  activeView={activeView}
                  canManage={canManageChannels}
                  onClick={() => handleVoiceChannelClick(ch.id)}
                />
              ) : (
                <TextChannelRow
                  serverId={activeServerId}
                  channelId={ch.id}
                  channelName={ch.name}
                  canManage={canManageChannels}
                />
              )}
            </div>
          ))}

        {/* Category blocks */}
        {activeServerId &&
          grouped.blocks.map(({ category, children }) => {
            const collapsed = collapsedCategories.has(category.id);
            return (
              <div key={category.id} className="mt-3 first:mt-0">
                {dragId && dropBefore === category.id && <DropLine />}
                <div
                  data-reorder-id={category.id}
                  draggable={canManageChannels}
                  onClick={() => toggleCategory(category.id)}
                  className="group mb-0.5 flex cursor-pointer select-none items-center gap-1 px-2 py-1"
                >
                  <svg
                    width="10"
                    height="10"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    className={`shrink-0 text-text-muted transition-transform ${collapsed ? "-rotate-90" : ""}`}
                  >
                    <polyline points="6 9 12 15 18 9" />
                  </svg>
                  <h3 className="truncate font-channel font-mono text-section font-medium uppercase leading-none tracking-section text-text-muted transition-colors group-hover:text-text-secondary">
                    {category.name}
                  </h3>
                  {canManageChannels && (
                    <span
                      role="button"
                      title="Create channel in category"
                      onClick={(e) => {
                        e.stopPropagation();
                        setCreateTarget({
                          type: "text",
                          categoryId: category.id,
                          allowTypeChoice: true,
                        });
                      }}
                      className="ml-auto flex h-4 w-4 cursor-pointer items-center justify-center rounded-sm text-text-muted opacity-0 transition-all hover:bg-surface-hover hover:text-text-primary group-hover:opacity-100"
                    >
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                        <line x1="12" y1="5" x2="12" y2="19" />
                        <line x1="5" y1="12" x2="19" y2="12" />
                      </svg>
                    </span>
                  )}
                </div>
                {!collapsed &&
                  children.map((ch) => (
                    <div key={ch.id}>
                      {dragId && dropBefore === ch.id && <DropLine />}
                      {ch.type === "voice" ? (
                        <VoiceRow
                          channel={ch}
                          presence={channelPresence[ch.id] ?? []}
                          connectedChannelId={connectedChannelId}
                          activeView={activeView}
                          canManage={canManageChannels}
                          onClick={() => handleVoiceChannelClick(ch.id)}
                        />
                      ) : (
                        <TextChannelRow
                          serverId={activeServerId}
                          channelId={ch.id}
                          channelName={ch.name}
                          canManage={canManageChannels}
                        />
                      )}
                    </div>
                  ))}
              </div>
            );
          })}

        {dragId && dropBefore === "END" && <DropLine />}

        {channels.length === 0 && (
          <p className="px-2 text-xs text-text-muted">No channels</p>
        )}
      </div>

      {/* Channel-list context menu (MANAGE_CHANNELS only) */}
      {ctxMenu && (
        <div
          className="fixed z-50 w-[190px] animate-[dropIn_0.12s_ease] rounded-md border border-border bg-bg-light p-[5px] shadow-float"
          style={{
            left: Math.min(ctxMenu.x, window.innerWidth - 200),
            top: Math.min(ctxMenu.y, window.innerHeight - 170),
          }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {ctxMenu.target.kind === "empty" ? (
            <>
              <ContextMenuItem
                label="Create Text Channel"
                onClick={() => {
                  setCtxMenu(null);
                  setCreateTarget({ type: "text" });
                }}
              />
              <ContextMenuItem
                label="Create Voice Channel"
                onClick={() => {
                  setCtxMenu(null);
                  setCreateTarget({ type: "voice" });
                }}
              />
              <ContextMenuItem
                label="Create Category"
                onClick={() => {
                  setCtxMenu(null);
                  setCreateTarget({ type: "category" });
                }}
              />
            </>
          ) : (
            <>
              <ContextMenuItem
                label={
                  ctxMenu.target.kind === "category"
                    ? "Category Settings"
                    : "Channel Settings"
                }
                onClick={() => {
                  const id = ctxMenu.target.kind !== "empty" ? ctxMenu.target.id : "";
                  setCtxMenu(null);
                  if (id) useUiStore.getState().openChannelSettings(id);
                }}
              />
              <ContextMenuItem
                label={
                  ctxMenu.target.kind === "category"
                    ? "Delete Category"
                    : "Delete Channel"
                }
                danger
                onClick={() => {
                  if (ctxMenu.target.kind === "empty") return;
                  const t = ctxMenu.target;
                  setCtxMenu(null);
                  setDeleteTarget({
                    id: t.id,
                    name: t.name,
                    isCategory: t.kind === "category",
                  });
                }}
              />
            </>
          )}
        </div>
      )}

      {/* Delete confirmation (context-menu path) */}
      {deleteTarget && activeServerId && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/65"
          onClick={() => setDeleteTarget(null)}
        >
          <div
            className="w-full max-w-sm animate-[cardIn_0.2s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 font-display text-[16px] font-semibold text-text-primary">
              {deleteTarget.isCategory
                ? `Delete category "${deleteTarget.name}"?`
                : `Delete #${deleteTarget.name}?`}
            </h3>
            <p className="mb-5 text-[13px] leading-[1.55] text-text-secondary">
              {deleteTarget.isCategory
                ? "Its channels stay and move up to the group above."
                : "Every message and attachment in it will be permanently deleted. Cannot be undone."}
            </p>
            <div className="flex gap-2.5">
              <button
                onClick={() => setDeleteTarget(null)}
                className="flex-1 cursor-pointer rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
              >
                Cancel
              </button>
              <button
                onClick={() => {
                  invoke("delete_channel", {
                    serverId: activeServerId,
                    channelId: deleteTarget.id,
                  }).catch(console.error);
                  setDeleteTarget(null);
                }}
                className="flex-1 cursor-pointer rounded-md bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85"
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}
      <div
        onMouseDown={onResizeMouseDown}
        className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/40 active:bg-accent/60"
      />
      {activeModal === "server-settings" && activeServerId && (
        <ServerSettingsModal serverId={activeServerId} />
      )}
      {activeModal === "leave-server-confirm" && leavePending && (
        <LeaveServerConfirmModal
          serverName={leavePending.name}
          onConfirm={() => {
            invoke("leave_server", { serverId: leavePending.id }).catch(
              console.error,
            );
            useChatStore.getState().removeConnectedServer(leavePending.id);
            useChatStore.getState().removePendingMembership(leavePending.id);
            useChatStore.getState().setActiveServer(null);
            setActiveChannel(null);
            setActiveView("home");
            setLeavePending(null);
          }}
        />
      )}
      {createTarget && activeServerId && (
        <CreateChannelModal
          serverId={activeServerId}
          channelType={createTarget.type}
          categoryId={createTarget.categoryId}
          allowTypeChoice={createTarget.allowTypeChoice}
          onClose={() => setCreateTarget(null)}
        />
      )}
    </div>
  );
}

// Per-channel row in the text-channels list. Self-sufficient: takes
// only stable string props (serverId, channelId, channelName) and
// pulls everything else — hover state, drag state, active state,
// upload progress — directly from the stores via primitive selectors.
//
// This shape is what makes memoization actually work. If we accepted
// e.g. an `onClick` prop, the parent would re-create that closure on
// every render and `memo`'s shallow check would always fail. With
// only stable strings as props, `memo` skips re-rendering rows whose
// store-derived state (returned as primitives via the selectors
// below) hasn't changed. Practically: when the user moves a dragged
// file from channel A to B, only A and B re-render (their hovered
// boolean flipped). Every other channel skips entirely. CSS class
// transitions therefore happen on real hover changes, not on every
// 60Hz dragover, which is what makes the `dropTargetIn` animation
// reliably restart instead of getting "swallowed" by React batching.
interface TextChannelRowProps {
  serverId: string;
  channelId: string;
  channelName: string;
  /// MANAGE_CHANNELS — shows the hover gear that opens this channel's
  /// settings. Computed once in the parent so the memoized row doesn't
  /// subscribe to the roles/members slices itself.
  canManage: boolean;
}

const TextChannelRow = memo(function TextChannelRow({
  serverId,
  channelId,
  channelName,
  canManage,
}: TextChannelRowProps) {
  const dropKey = `channel:${serverId}:${channelId}`;
  // Per-row primitive selectors — each returns a boolean / number, so
  // zustand's Object.is equality means we only re-render when our
  // own derived value flips.
  //
  // CRITICAL: never combine hook calls with `&&` short-circuit —
  // `useChatStore(...) && useUiStore(...)` would skip the second
  // hook when the first returned false, violating the rules of
  // hooks (different number of hooks across renders → React tears
  // the tree down with "Rendered fewer hooks than expected"). Bind
  // each hook to its own const, then combine the values.
  const isActiveChannel = useChatStore((s) => s.activeChannelId === channelId);
  const isServerView = useUiStore((s) => s.activeView === "server");
  const isActive = isActiveChannel && isServerView;
  const isHoveredDrop = useUiStore((s) => s.dragHoveredKey === dropKey);
  const dragActive = useUiStore((s) => s.dragActive);
  // Aggregate live upload progress for this channel: sum of bytes
  // transferred across all `uploading` pendings, divided by total.
  // Returns null when nothing is uploading so the row reverts cleanly.
  const progress = useAttachmentsStore((s) => {
    let total = 0;
    let done = 0;
    for (const p of Object.values(s.pendings)) {
      if (p.serverId === serverId && p.channelId === channelId && p.status === "uploading") {
        total += p.totalBytes;
        done += p.transferredBytes;
      }
    }
    return total > 0 ? Math.min(1, done / total) : null;
  });

  const onClick = () => {
    // Click handler defined inline — only fires on actual user click,
    // not on render, so a fresh closure ref per render is harmless
    // (it's never compared against anything). The active-channel +
    // active-view writes are gated so a click on the already-active
    // channel doesn't churn the store.
    const chat = useChatStore.getState();
    const ui = useUiStore.getState();
    if (chat.activeChannelId !== channelId) chat.setActiveChannel(channelId);
    if (ui.activeView !== "server") ui.setActiveView("server");
  };
  return (
    <button
      onClick={onClick}
      data-drop-target={dropKey}
      data-server-id={serverId}
      data-channel-id={channelId}
      data-reorder-id={channelId}
      draggable={canManage}
      className={`list-row group relative flex w-full cursor-pointer items-center rounded-sm text-channel transition-all ${
        isHoveredDrop
          ? "animate-[dropTargetIn_0.18s_ease_both] bg-accent text-on-accent"
          : isActive
            ? "bg-accent-soft text-text-bright font-semibold"
            : dragActive
              ? "animate-[dropPulse_1.6s_ease-in-out_infinite] bg-accent-soft/30 text-text-secondary"
              : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
      }`}
    >
      <span
        className={`font-channel text-channel font-medium transition-colors ${
          isHoveredDrop
            ? "text-on-accent"
            : isActive
              ? "text-accent-bright"
              : dragActive
                ? "text-accent"
                : "text-text-muted"
        }`}
      >
        #
      </span>
      <span className="truncate font-channel">{channelName}</span>

      {/* Settings gear — appears on row hover (Discord-style) for
          MANAGE_CHANNELS holders and opens this channel's settings
          without needing to activate the channel first. Rendered as a
          span (a real <button> can't nest inside the row button).
          Hidden while a file drag is in flight so it doesn't fight
          the upload-target icon for the ml-auto slot. */}
      {canManage && !dragActive && (
        <span
          role="button"
          title="Channel settings"
          onClick={(e) => {
            e.stopPropagation();
            useUiStore.getState().openChannelSettings(channelId);
          }}
          className="ml-auto hidden shrink-0 cursor-pointer rounded-sm p-0.5 text-text-muted opacity-0 transition-all hover:text-text-primary group-hover:block group-hover:opacity-100"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </span>
      )}

      {/* Upload-target hint icon — appears at the end of every text
          channel row while a file drag is in flight, mirroring the
          icon that lights up over the message input. The hovered drop
          target gets a brighter accent so the user can see exactly
          which channel they're about to drop on. */}
      {dragActive && (
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`ml-auto shrink-0 ${
            isHoveredDrop ? "text-on-accent" : "text-accent"
          }`}
        >
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="17 8 12 3 7 8" />
          <line x1="12" y1="3" x2="12" y2="15" />
        </svg>
      )}

      {/* Live upload-progress bar — pinned to the bottom edge of the
          row while any pendings for this channel are uploading. Tiny
          height + accent fill so it reads as "this channel has
          something happening" without disturbing the row layout. */}
      {progress !== null && (
        <div className="pointer-events-none absolute inset-x-1.5 bottom-0.5 h-[2px] overflow-hidden rounded-full bg-accent-soft">
          <div
            className="h-full rounded-full bg-accent transition-[width] duration-150 ease-out"
            style={{ width: `${Math.round(progress * 100)}%` }}
          />
        </div>
      )}
    </button>
  );
});

/// 2px accent insertion indicator shown between rows during a
/// drag-reorder.
function DropLine() {
  return <div className="mx-1.5 my-0.5 h-0.5 rounded-full bg-accent" />;
}

function ContextMenuItem({
  label,
  danger = false,
  onClick,
}: {
  label: string;
  danger?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex w-full cursor-pointer items-center rounded-sm px-2.5 py-[8px] text-[13px] transition-colors ${
        danger
          ? "text-text-muted hover:bg-error/10 hover:text-error"
          : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
      }`}
    >
      {label}
    </button>
  );
}

/// Voice channel row + its participant lists. Extracted from the old
/// inline voice-section map so the flat category renderer can place
/// voice rows anywhere in the list.
function VoiceRow({
  channel,
  presence,
  connectedChannelId,
  activeView,
  canManage,
  onClick,
}: {
  channel: ChannelInfo;
  presence: string[];
  connectedChannelId: string | null;
  activeView: string;
  canManage: boolean;
  onClick: () => void;
}) {
  const connected = connectedChannelId === channel.id;
  return (
    <div>
      <button
        onClick={onClick}
        data-reorder-id={channel.id}
        draggable={canManage}
        className={`list-row group flex w-full cursor-pointer items-center rounded-sm text-channel transition-colors ${
          connected && activeView === "voice"
            ? "bg-accent-soft text-text-bright font-semibold"
            : connected
              ? "text-success font-semibold hover:bg-surface-hover"
              : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
        }`}
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`shrink-0 ${
            connected && activeView === "voice"
              ? "text-accent"
              : connected
                ? "text-success"
                : "text-text-muted"
          }`}
        >
          <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
          <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
          <line x1="12" y1="19" x2="12" y2="23" />
          <line x1="8" y1="23" x2="16" y2="23" />
        </svg>
        <span className="truncate font-channel">{channel.name}</span>
        {canManage && (
          <span
            role="button"
            title="Channel settings"
            onClick={(e) => {
              e.stopPropagation();
              useUiStore.getState().openChannelSettings(channel.id);
            }}
            className="ml-auto hidden shrink-0 cursor-pointer rounded-sm p-0.5 text-text-muted opacity-0 transition-all hover:text-text-primary group-hover:block group-hover:opacity-100"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
          </span>
        )}
      </button>
      {connected ? (
        <VoiceParticipantList />
      ) : presence.length > 0 ? (
        <VoiceParticipantList usernames={presence} channelId={channel.id} />
      ) : null}
    </div>
  );
}
