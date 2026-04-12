import { memo, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useUiStore } from "../stores/uiStore";
import { useChatStore } from "../stores/chatStore";
import { useDmStore } from "../stores/dmStore";

const win = getCurrentWindow();

function Titlebar() {
  const [maximized, setMaximized] = useState(false);
  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const servers = useChatStore((s) => s.servers);
  const activeDmUser = useDmStore((s) => s.activeDmUser);

  let title = "Decibell";
  if (activeView === "dm" && activeDmUser) {
    title = activeDmUser;
  } else if ((activeView === "server" || activeView === "voice") && activeServerId) {
    title = servers.find((s) => s.id === activeServerId)?.name ?? "Decibell";
  }

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    win.isMaximized().then((v) => { if (!disposed) setMaximized(v); }).catch(() => {});
    win.onResized(() => {
      win.isMaximized().then((v) => { if (!disposed) setMaximized(v); }).catch(() => {});
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch(() => {});
    return () => { disposed = true; unlisten?.(); };
  }, []);

  return (
    <div
      data-tauri-drag-region
      onDoubleClick={() => { win.toggleMaximize().catch(() => {}); }}
      className="relative flex h-8 shrink-0 select-none items-center justify-center border-b border-border bg-bg-titlebar"
    >
      <span
        data-tauri-drag-region
        className="pointer-events-none text-[12px] font-medium text-text-secondary"
        style={{
          fontFamily: '"Segoe UI Variable", "Segoe UI", system-ui, -apple-system, "SF Pro Text", "Helvetica Neue", sans-serif',
          letterSpacing: 0,
        }}
      >
        {title}
      </span>
      <div className="absolute right-0 top-0 flex h-full">
        <TitleButton onClick={() => win.minimize().catch(() => {})} label="Minimize">
          <svg width="14" height="14" viewBox="0 0 12 12" shapeRendering="crispEdges">
            <line x1="2" y1="6" x2="10" y2="6" stroke="currentColor" strokeWidth="1" />
          </svg>
        </TitleButton>
        <TitleButton onClick={() => win.toggleMaximize().catch(() => {})} label={maximized ? "Restore" : "Maximize"}>
          {maximized ? (
            <svg width="14" height="14" viewBox="0 0 12 12" shapeRendering="crispEdges">
              <rect x="2.5" y="3.5" width="6" height="6" fill="none" stroke="currentColor" strokeWidth="1" />
              <path d="M4.5 3.5 V2.5 H10.5 V8.5 H9.5" fill="none" stroke="currentColor" strokeWidth="1" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 12 12" shapeRendering="crispEdges">
              <rect x="2" y="2" width="8" height="8" fill="none" stroke="currentColor" strokeWidth="1" />
            </svg>
          )}
        </TitleButton>
        <TitleButton onClick={() => win.close().catch(() => {})} label="Close" danger>
          <svg width="14" height="14" viewBox="0 0 12 12">
            <path d="M2.5 2.5 L9.5 9.5 M9.5 2.5 L2.5 9.5" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
          </svg>
        </TitleButton>
      </div>
    </div>
  );
}

function TitleButton({ onClick, label, danger, children }: {
  onClick: () => void;
  label: string;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      aria-label={label}
      title={label}
      className={`flex h-full w-11 items-center justify-center text-text-secondary transition-colors ${
        danger ? "hover:bg-error hover:text-white" : "hover:bg-white/[0.08] hover:text-text-primary"
      }`}
    >
      {children}
    </button>
  );
}

export default memo(Titlebar);
