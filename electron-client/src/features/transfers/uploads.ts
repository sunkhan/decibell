import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { useChatStore } from "../../stores/chatStore";
import { openServer } from "../servers/openServer";
import {
  useTransfersStore,
  type TransfersAttention,
  type UploadEntry,
  type UploadState,
} from "../../stores/transfersStore";

// The Uploads tab's history. attachmentsStore holds a pending only until
// its message is sent; this mirrors every pending that has started
// sending into transfersStore, where the row stays (with its last state)
// for the rest of the session.

const SPEED_SAMPLE_MS = 400;

/// Speed sampling per upload, outside the store so it never renders.
const samples = new Map<string, { at: number; bytes: number; speed: number }>();

function stateOf(p: PendingAttachment): UploadState | null {
  if (p.status === "queued" || p.status === "pending") return null;
  if (p.status === "failed") return p.cancelled ? "cancelled" : "failed";
  if (p.status === "ready") return "done";
  return p.paused ? "paused" : "uploading";
}

function speedOf(pendingId: string, bytes: number, live: boolean): number {
  if (!live) {
    samples.delete(pendingId);
    return 0;
  }
  const now = performance.now();
  const s = samples.get(pendingId);
  if (!s) {
    samples.set(pendingId, { at: now, bytes, speed: 0 });
    return 0;
  }
  const dt = now - s.at;
  if (dt >= SPEED_SAMPLE_MS && bytes !== s.bytes) {
    const inst = ((bytes - s.bytes) * 1000) / dt;
    s.speed = s.speed === 0 ? inst : s.speed * 0.7 + inst * 0.3;
    s.at = now;
    s.bytes = bytes;
  }
  return s.speed;
}

function names(serverId: string, channelId: string): { serverName: string; channelName: string } {
  const chat = useChatStore.getState();
  return {
    serverName: chat.servers.find((s) => s.id === serverId)?.name ?? "",
    channelName: chat.channelsByServer[serverId]?.find((c) => c.id === channelId)?.name ?? "",
  };
}

function sync(pendings: Record<string, PendingAttachment>): void {
  const { uploads } = useTransfersStore.getState();
  const byId = new Map(uploads.map((u) => [u.pendingId, u]));
  const added: UploadEntry[] = [];
  let changed = false;
  let attention: TransfersAttention = "none";
  const now = Date.now();

  const updated = new Map<string, UploadEntry>();
  for (const p of Object.values(pendings)) {
    const state = stateOf(p);
    if (!state) continue;
    const old = byId.get(p.pendingId);
    const live = state === "uploading";
    const speedBps = speedOf(p.pendingId, p.transferredBytes, live);
    if (
      old &&
      old.state === state &&
      old.transferredBytes === p.transferredBytes &&
      old.previewUrl === p.previewUrl &&
      old.attachmentId === p.attachmentId &&
      old.speedBps === speedBps
    ) {
      continue;
    }
    const finished = state === "done" || state === "failed" || state === "cancelled";
    if (state === "failed" && old?.state !== "failed") attention = "failed";
    const entry: UploadEntry = {
      pendingId: p.pendingId,
      serverId: p.serverId,
      channelId: p.channelId,
      ...(old ? { serverName: old.serverName, channelName: old.channelName } : names(p.serverId, p.channelId)),
      filename: p.filename,
      kind: p.kind,
      previewUrl: p.previewUrl,
      attachmentId: p.attachmentId,
      totalBytes: p.totalBytes,
      transferredBytes: p.transferredBytes,
      state,
      error: state === "failed" ? p.errorMessage ?? "Upload failed" : null,
      speedBps,
      startedAt: old?.startedAt ?? now,
      finishedAt: finished ? old?.finishedAt ?? now : null,
    };
    if (old) updated.set(p.pendingId, entry);
    else added.push(entry);
    changed = true;
  }

  // Rows whose pending is gone: the blob preview was revoked with it,
  // and one that vanished mid-flight didn't finish.
  const next = uploads.map((u) => {
    const fresh = updated.get(u.pendingId);
    if (fresh) return fresh;
    if (pendings[u.pendingId]) return u;
    if (u.previewUrl === null && u.state !== "uploading" && u.state !== "paused") return u;
    changed = true;
    samples.delete(u.pendingId);
    const unfinished = u.state === "uploading" || u.state === "paused";
    return {
      ...u,
      previewUrl: null,
      speedBps: 0,
      state: unfinished ? ("cancelled" as const) : u.state,
      finishedAt: u.finishedAt ?? now,
    };
  });

  if (!changed) return;
  useTransfersStore.getState().setUploads([...added.reverse(), ...next], attention, added.length > 0);
}

let initialised = false;

/// Follow attachmentsStore for the life of the window.
export function initUploads(): void {
  if (initialised) return;
  initialised = true;
  useAttachmentsStore.subscribe((state, prev) => {
    if (state.pendings !== prev.pendings) sync(state.pendings);
  });
}

export function pauseUpload(pendingId: string, paused: boolean): void {
  useAttachmentsStore.getState().setPaused(pendingId, paused);
}

export function cancelUpload(pendingId: string): void {
  useAttachmentsStore.getState().pendings[pendingId]?.abortController.abort();
}

/// Open the channel an upload went to.
export function goToChannel(serverId: string, channelId: string): void {
  openServer(serverId);
  const chat = useChatStore.getState();
  if (chat.channelsByServer[serverId]?.some((c) => c.id === channelId)) {
    chat.setActiveChannel(channelId);
  }
}
