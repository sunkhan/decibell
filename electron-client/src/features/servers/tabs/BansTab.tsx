import { useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { UserAvatar } from "../../../components/UserAvatar";
import { PERM, usePermission } from "../permissions";
import { formatJoined } from "./helpers";

/// Bans: the ban list (only BAN_MEMBERS holders receive it from the
/// server) with reason / moderator / expiry and unban. The list refresh
/// after a successful unban arrives via the pushed ban_list_received.
export default function BansTab({ serverId }: { serverId: string }) {
  const bans = useChatStore((s) => s.bansByServer[serverId] ?? []);
  const canBan = usePermission(serverId, PERM.BAN_MEMBERS);
  const [error, setError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);

  const runUnban = async (username: string) => {
    setPendingAction(`unban:${username}`);
    setError(null);
    try {
      await invoke("unban_member", { serverId, username });
    } catch (err) {
      setError(String(err));
    } finally {
      setPendingAction(null);
    }
  };

  return (
    <div className="flex flex-col">
      {error && <p className="mb-3 text-[12px] text-error">{error}</p>}

      {bans.length === 0 ? (
        <div className="flex flex-col items-center gap-2.5 py-8 text-text-muted">
          <div className="flex h-10 w-10 items-center justify-center rounded-full bg-text-muted/10">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="12" cy="12" r="10" />
              <line x1="4.93" y1="4.93" x2="19.07" y2="19.07" />
            </svg>
          </div>
          <span className="text-[13px]">No bans.</span>
        </div>
      ) : (
        <div className="flex flex-col gap-1">
          {bans.map((b) => (
            <div
              key={b.username}
              className="flex items-center gap-3 rounded-md px-3 py-2.5 transition-colors hover:bg-surface-hover"
            >
              <div className="opacity-50">
                <UserAvatar username={b.username} size={36} />
              </div>
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] font-medium text-text-secondary">
                  {b.username}
                </div>
                <div className="truncate text-[11px] text-text-muted">
                  {b.bannedBy ? `by ${b.bannedBy}` : ""}
                  {b.bannedAt ? ` · ${formatJoined(b.bannedAt)}` : ""}
                  {b.expiresAt
                    ? ` · until ${new Date(b.expiresAt * 1000).toLocaleString()}`
                    : " · permanent"}
                  {b.reason ? ` · ${b.reason}` : ""}
                </div>
              </div>
              {canBan && (
                <button
                  onClick={() => runUnban(b.username)}
                  disabled={pendingAction === `unban:${b.username}`}
                  className="rounded-sm bg-bg-light px-2.5 py-1.5 text-[11px] font-medium text-text-muted transition-colors hover:bg-success/15 hover:text-success disabled:opacity-50"
                >
                  Unban
                </button>
              )}
              <span className="rounded-sm bg-error/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-error">
                {b.expiresAt ? "Temp ban" : "Banned"}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
