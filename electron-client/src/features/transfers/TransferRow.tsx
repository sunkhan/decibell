import { useState, type ReactNode } from "react";
import type { AttachmentKind } from "../../types";
import { KindIcon } from "./icons";

export interface RowAction {
  label: string;
  icon: ReactNode;
  onClick: () => void;
}

/// One line of the Transfers panel, shared by downloads and uploads:
/// preview, name, optional progress bar, status line, icon actions.
/// Finished rows hide their actions until hover / keyboard focus
/// (opacity, not visibility, so they stay focusable).
export default function TransferRow({
  preview,
  name,
  struck = false,
  status,
  error = false,
  progress = null,
  paused = false,
  actions,
  persistentActions,
  onActivate,
  activateLabel,
  onContextMenu,
}: {
  preview: ReactNode;
  name: string;
  struck?: boolean;
  status: string;
  error?: boolean;
  /// 0–1, or null for no bar.
  progress?: number | null;
  paused?: boolean;
  actions: RowAction[];
  persistentActions: boolean;
  onActivate?: () => void;
  activateLabel?: string;
  onContextMenu?: (e: React.MouseEvent) => void;
}) {
  return (
    <div
      role={onActivate ? "button" : undefined}
      tabIndex={onActivate ? 0 : undefined}
      aria-label={onActivate ? activateLabel : undefined}
      title={onActivate ? activateLabel : undefined}
      onClick={onActivate}
      onKeyDown={(e) => {
        if (onActivate && e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
          e.preventDefault();
          onActivate();
        }
      }}
      onContextMenu={onContextMenu}
      className={`group flex items-center gap-3 rounded-md px-2 py-2 outline-none transition-colors hover:bg-surface-hover focus-visible:bg-surface-hover ${
        onActivate ? "cursor-pointer" : ""
      }`}
    >
      <div className="flex h-9 w-9 shrink-0 items-center justify-center overflow-hidden rounded-sm bg-bg-mid text-text-muted">
        {preview}
      </div>
      <div className="min-w-0 flex-1">
        <div
          className={`truncate text-[13px] font-medium ${
            struck ? "text-text-muted line-through" : "text-text-primary"
          }`}
        >
          {name}
        </div>
        {progress !== null && (
          <div className="mt-1 h-1 overflow-hidden rounded-full bg-bg-darkest">
            <div
              className={`h-full rounded-full transition-[width] duration-300 ${paused ? "bg-text-muted" : "bg-accent"}`}
              style={{ width: `${Math.round(Math.max(0, Math.min(1, progress)) * 100)}%` }}
            />
          </div>
        )}
        <div className={`mt-0.5 truncate text-[11px] ${error ? "text-error" : "text-text-muted"}`}>{status}</div>
      </div>
      {actions.length > 0 && (
        <div
          className={`flex shrink-0 items-center gap-0.5 transition-opacity ${
            persistentActions ? "" : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100"
          }`}
        >
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              aria-label={a.label}
              title={a.label}
              onClick={(e) => {
                e.stopPropagation();
                a.onClick();
              }}
              onKeyDown={(e) => e.stopPropagation()}
              className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-active hover:text-text-primary"
            >
              {a.icon}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/// Thumbnail when there is one to show, the kind glyph otherwise (or
/// when the image fails: offline server, no thumbnail variant).
export function TransferPreview({ src, kind }: { src: string | null; kind: AttachmentKind }) {
  const [failed, setFailed] = useState(false);
  if (src && !failed) {
    return (
      <img
        src={src}
        alt=""
        draggable={false}
        onError={() => setFailed(true)}
        className="h-full w-full object-cover"
      />
    );
  }
  return <KindIcon kind={kind} />;
}
