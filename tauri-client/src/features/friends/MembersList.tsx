import { useMemo } from "react";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";

export default function MembersList() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  const membersByServer = useChatStore((s) => s.membersByServer);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  const { online, offline } = useMemo(() => {
    const roster = activeServerId ? membersByServer[activeServerId] ?? [] : [];
    const onlineList = roster.filter((m) => m.isOnline);
    const offlineList = roster.filter((m) => !m.isOnline);
    const byName = (a: { username: string }, b: { username: string }) =>
      a.username.localeCompare(b.username);
    onlineList.sort(byName);
    offlineList.sort(byName);
    return { online: onlineList, offline: offlineList };
  }, [activeServerId, membersByServer]);

  const renderRow = (username: string, isOnline: boolean) => (
    <div
      key={username}
      className="flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-[7px] transition-colors hover:bg-surface-hover"
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(username, { x: rect.right + 8, y: rect.top }, activeServerId);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY });
      }}
    >
      <div className="relative shrink-0">
        <div
          className={`flex h-[34px] w-[34px] items-center justify-center rounded-lg text-[13px] font-bold text-white ${
            isOnline ? "" : "opacity-50"
          }`}
          style={{ background: stringToGradient(username) }}
        >
          {username.charAt(0).toUpperCase()}
        </div>
        <div
          className={`absolute -bottom-px -right-px h-[11px] w-[11px] rounded-full border-[2.5px] border-bg-tertiary ${
            isOnline ? "bg-success" : "bg-text-muted"
          }`}
        />
      </div>
      <span
        className={`truncate font-channel text-[13px] font-medium ${
          isOnline ? "text-text-primary" : "text-text-muted"
        }`}
      >
        {username}
      </span>
    </div>
  );

  return (
    <div className="flex w-[260px] shrink-0 flex-col border-l border-border bg-bg-dark">
      <div className="flex-1 overflow-y-auto px-3 py-1">
        <div className="px-1 pt-3 pb-1">
          <h3 className="font-channel text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Online — {online.length}
          </h3>
        </div>
        {online.map((m) => renderRow(m.username, true))}

        {offline.length > 0 && (
          <>
            <div className="px-1 pt-4 pb-1">
              <h3 className="font-channel text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                Offline — {offline.length}
              </h3>
            </div>
            {offline.map((m) => renderRow(m.username, false))}
          </>
        )}

        {online.length === 0 && offline.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No members yet
          </p>
        )}
      </div>

      {/* Brand mark */}
      <div className="flex items-center gap-2 px-4 pb-3 pt-2">
        <div className="flex items-end gap-[2px] opacity-25" style={{ height: 16 }}>
          {[0, 0.15, 0.3, 0.45, 0.6].map((delay, i) => (
            <div
              key={i}
              className="w-[3px] rounded-[2px] bg-accent animate-[waveBar_1.2s_ease-in-out_infinite]"
              style={{ animationDelay: `${delay}s` }}
            />
          ))}
        </div>
        <span className="font-display text-[11px] font-medium uppercase tracking-[0.12em] text-text-faint opacity-50">
          Decibell
        </span>
      </div>
    </div>
  );
}
