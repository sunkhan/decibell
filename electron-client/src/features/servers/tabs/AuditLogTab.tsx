import { useCallback, useEffect, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { UserAvatar } from "../../../components/UserAvatar";
import type { AuditEntry, AuditLogReceivedPayload } from "../../../types";

const ACTION_LABELS: Record<string, string> = {
  kick: "kicked",
  ban: "banned",
  unban: "unbanned",
  timeout: "timed out",
  timeout_clear: "cleared the timeout of",
  role_create: "created role",
  role_update: "updated role",
  role_delete: "deleted role",
  member_roles: "changed roles of",
  channel_create: "created channel",
  channel_rename: "renamed channel to",
  channel_delete: "deleted channel",
  channel_update: "updated channel",
  channel_wipe: "wiped channel",
  message_delete: "deleted a message by",
  overwrite_set: "changed channel permissions for",
  nickname: "changed the nickname of",
  server_update: "updated the server",
  ownership_transfer: "transferred ownership to",
  voice_mod: "voice-moderated",
  invite_create: "created invite",
  invite_revoke: "revoked invite",
};

function describe(e: AuditEntry, channelName: (id: string) => string): string {
  const verb = ACTION_LABELS[e.action] ?? e.action.replace(/_/g, " ");
  const parts = [verb];
  if (e.target) parts.push(e.target);
  if (e.channelId) parts.push(`in #${channelName(e.channelId)}`);
  return parts.join(" ");
}

/// Audit log (VIEW_AUDIT_LOG): newest first, paged by id. Entries come
/// from the server's audit_log table written by every moderation and
/// management handler.
export default function AuditLogTab({ serverId }: { serverId: string }) {
  const channels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchPage = useCallback(
    (beforeId: number) => {
      setLoading(true);
      invoke("list_audit_log", { serverId, beforeId, limit: 50 }).catch((err) => {
        setLoading(false);
        setError(String(err));
      });
    },
    [serverId],
  );

  useEffect(() => {
    setEntries([]);
    fetchPage(0);
  }, [fetchPage]);

  useEffect(() => {
    const un = listen<AuditLogReceivedPayload>("audit_log_received", (event) => {
      const p = event.payload;
      if (p.serverId !== serverId) return;
      setLoading(false);
      if (!p.success) {
        setError(p.message || "Couldn't load the audit log.");
        return;
      }
      setError(null);
      setEntries((prev) => {
        // Page 1 replaces; later pages (all ids below ours) append.
        const minId = prev.length ? prev[prev.length - 1].id : Infinity;
        const append = p.entries.length > 0 && p.entries[0].id < minId;
        return append ? [...prev, ...p.entries] : p.entries;
      });
      setHasMore(p.hasMore);
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId]);

  return (
    <div className="flex flex-col">
      {error && <p className="mb-3 text-[12px] text-error">{error}</p>}
      {entries.length === 0 && !loading ? (
        <div className="flex flex-col items-center gap-2.5 py-8 text-text-muted">
          <span className="text-[13px]">No entries yet.</span>
        </div>
      ) : (
        <div className="flex flex-col divide-y divide-border-divider rounded-md border border-border">
          {entries.map((e) => (
            <div key={e.id} className="flex items-start gap-3 px-3 py-2.5">
              <UserAvatar username={e.actor} size={28} />
              <div className="min-w-0 flex-1">
                <div className="text-[13px] text-text-primary">
                  <span className="font-medium">{e.actor}</span>{" "}
                  <span className="text-text-secondary">{describe(e, channelName)}</span>
                </div>
                {e.details && (
                  <div className="truncate text-[11px] text-text-muted">{e.details}</div>
                )}
              </div>
              <div className="shrink-0 text-[11px] text-text-muted">
                {new Date(e.timestamp * 1000).toLocaleString()}
              </div>
            </div>
          ))}
        </div>
      )}
      {hasMore && (
        <button
          type="button"
          onClick={() => fetchPage(entries[entries.length - 1]?.id ?? 0)}
          disabled={loading}
          className="mt-2 w-full rounded-md bg-bg-light py-2 text-[12px] text-text-secondary transition-colors hover:bg-bg-lighter disabled:opacity-60"
        >
          {loading ? "Loading…" : "Load older"}
        </button>
      )}
    </div>
  );
}
