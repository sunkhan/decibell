// Drag a voice participant onto another voice channel in the sidebar to
// move them (Discord-style). Same gate and wire request as the context
// menu's "Move to…": MOVE_MEMBERS in the member's current channel (the
// owner always passes) plus the hierarchy (above them, or a peer with
// "members can manage each other"). The server enforces both regardless,
// and its refusals toast through mod_action_responded.

import { create } from "zustand";
import type React from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { toast } from "../../stores/toastStore";
import { PERM, canManageMember, hasBits } from "../servers/permissions";

const MIME = "application/x-decibell-voice-member";

interface MemberDrag {
  serverId: string;
  /// The channel the member is in — not a valid drop target.
  channelId: string;
  username: string;
}

/// `hover` is the voice channel under the cursor while a drag is in flight.
export const useMemberDragStore = create<{ drag: MemberDrag | null; hover: string | null }>(() => ({
  drag: null,
  hover: null,
}));

const clear = () => {
  if (useMemberDragStore.getState().drag) useMemberDragStore.setState({ drag: null, hover: null });
};

// dragend fires on the source row; if that row unmounted mid-drag (the
// member left voice) it never reaches us, so also end on any drop / dragend
// the window sees.
let windowListeners = false;
function ensureWindowListeners() {
  if (windowListeners) return;
  windowListeners = true;
  window.addEventListener("dragend", clear, true);
  window.addEventListener("drop", () => setTimeout(clear, 0), true);
}

export function canMoveMember(serverId: string, channelId: string, username: string): boolean {
  const me = useAuthStore.getState().username;
  if (!me || username === me) return false;
  const chat = useChatStore.getState();
  const owner = chat.serverOwner[serverId];
  if (username === owner) return false;
  if (owner !== me) {
    const perms = chat.channelsByServer[serverId]?.find((c) => c.id === channelId)?.myPermissions ?? 0;
    if (!hasBits(perms, PERM.MOVE_MEMBERS)) return false;
  }
  return canManageMember(serverId, username);
}

/// dragstart on a participant row. Rows are only `draggable` where we hold
/// MOVE_MEMBERS; the hierarchy is checked here (a refused drag never
/// starts), so rows don't each subscribe to the whole member list.
export function onMemberDragStart(
  e: React.DragEvent,
  serverId: string,
  channelId: string,
  username: string,
) {
  // The channel list delegates its reorder drag on the container and
  // cancels any drag that didn't start on a channel row.
  e.stopPropagation();
  if (!canMoveMember(serverId, channelId, username)) {
    e.preventDefault();
    return;
  }
  ensureWindowListeners();
  e.dataTransfer.setData(MIME, username);
  e.dataTransfer.effectAllowed = "move";
  useMemberDragStore.setState({ drag: { serverId, channelId, username }, hover: null });
}

export const onMemberDragEnd = clear;

function isTarget(drag: MemberDrag | null, serverId: string, channelId: string): drag is MemberDrag {
  return !!drag && drag.serverId === serverId && drag.channelId !== channelId;
}

/// Selector: this voice channel accepts the drag in flight.
export const acceptsMemberDrop = (serverId: string, channelId: string) =>
  (s: { drag: MemberDrag | null }) => isTarget(s.drag, serverId, channelId);

/// Drop-target handlers for one voice channel (its row + participant list).
export function memberDropHandlers(serverId: string, channelId: string) {
  return {
    onDragOver: (e: React.DragEvent) => {
      const { drag, hover } = useMemberDragStore.getState();
      if (!isTarget(drag, serverId, channelId)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      if (hover !== channelId) useMemberDragStore.setState({ hover: channelId });
    },
    onDragLeave: (e: React.DragEvent) => {
      if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
      if (useMemberDragStore.getState().hover === channelId) {
        useMemberDragStore.setState({ hover: null });
      }
    },
    onDrop: (e: React.DragEvent) => {
      const { drag } = useMemberDragStore.getState();
      if (!isTarget(drag, serverId, channelId)) return;
      e.preventDefault();
      e.stopPropagation();
      useMemberDragStore.setState({ drag: null, hover: null });
      invoke("voice_mod", { serverId, username: drag.username, action: "move", channelId }).catch((err) =>
        toast.error("Couldn't move member", String(err)),
      );
    },
  };
}
