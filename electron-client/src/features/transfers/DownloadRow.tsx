import { formatFileSize } from "../chat/attachmentHelpers";
import type { DownloadView } from "../../types";
import TransferRow, { TransferPreview, type RowAction } from "./TransferRow";
import { formatAgo, formatEta, formatProgress, formatSpeed } from "./format";
import { CloseIcon, FolderIcon, PauseIcon, PlayIcon, RetryIcon } from "./icons";

const api = () => window.decibell.downloads;

function thumbSrc(d: DownloadView): string | null {
  if (d.kind !== "image" && d.kind !== "video") return null;
  return `decibell-attachment://attach/${encodeURIComponent(d.serverId)}/${d.attachmentId}?variant=thumb&size=320`;
}

export default function DownloadRow({
  d,
  now,
  onOpen,
  onContextMenu,
}: {
  d: DownloadView;
  now: number;
  onOpen: (d: DownloadView) => void;
  onContextMenu: (e: React.MouseEvent, d: DownloadView) => void;
}) {
  const pause: RowAction = { label: "Pause", icon: <PauseIcon />, onClick: () => void api().pause(d.id) };
  const resume: RowAction = { label: "Resume", icon: <PlayIcon />, onClick: () => void api().resume(d.id) };
  const cancel: RowAction = { label: "Cancel", icon: <CloseIcon />, onClick: () => void api().cancel(d.id) };
  const retry = (label: string): RowAction => ({
    label,
    icon: <RetryIcon />,
    onClick: () => void api().resume(d.id),
  });
  const remove: RowAction = { label: "Remove from list", icon: <CloseIcon />, onClick: () => void api().remove(d.id) };
  const folder: RowAction = {
    label: "Show in folder",
    icon: <FolderIcon />,
    onClick: () => void api().showInFolder(d.id),
  };
  const fraction = d.totalBytes > 0 ? d.receivedBytes / d.totalBytes : 0;
  const where = d.context.channelName ? `#${d.context.channelName}` : d.context.serverName;

  const common = {
    preview: <TransferPreview src={thumbSrc(d)} kind={d.kind} />,
    name: d.filename,
    onContextMenu: (e: React.MouseEvent) => onContextMenu(e, d),
  };

  switch (d.state) {
    case "active":
      return (
        <TransferRow
          {...common}
          status={[
            formatProgress(d.receivedBytes, d.totalBytes),
            d.speedBps > 0 ? formatSpeed(d.speedBps) : "",
            formatEta(d.etaS),
          ]
            .filter(Boolean)
            .join(" · ")}
          progress={fraction}
          actions={[pause, cancel]}
          persistentActions
        />
      );
    case "paused":
      return (
        <TransferRow
          {...common}
          status={`Paused · ${formatProgress(d.receivedBytes, d.totalBytes)}`}
          progress={fraction}
          paused
          actions={[resume, cancel]}
          persistentActions
        />
      );
    case "failed":
      return (
        <TransferRow
          {...common}
          status={`Failed — ${d.error ?? "unknown error"}`}
          error
          actions={[retry("Retry"), remove]}
          persistentActions
        />
      );
    case "cancelled":
      return (
        <TransferRow
          {...common}
          struck
          status="Cancelled"
          actions={[retry("Retry"), remove]}
          persistentActions={false}
        />
      );
    case "done":
      if (d.missing) {
        return (
          <TransferRow
            {...common}
            struck
            status="Deleted"
            actions={[retry("Download again"), remove]}
            persistentActions={false}
          />
        );
      }
      return (
        <TransferRow
          {...common}
          status={[formatFileSize(d.totalBytes), where, d.finishedAt ? formatAgo(d.finishedAt, now) : ""]
            .filter(Boolean)
            .join(" · ")}
          actions={[folder, remove]}
          persistentActions={false}
          onActivate={() => onOpen(d)}
          activateLabel={`Open ${d.filename}`}
        />
      );
  }
}
