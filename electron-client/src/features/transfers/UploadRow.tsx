import { formatFileSize } from "../chat/attachmentHelpers";
import { useTransfersStore, type UploadEntry } from "../../stores/transfersStore";
import TransferRow, { TransferPreview, type RowAction } from "./TransferRow";
import { formatProgress, formatSpeed } from "./format";
import { CloseIcon, PauseIcon, PlayIcon } from "./icons";
import { cancelUpload, goToMessage, messageIdForAttachment, pauseUpload } from "./uploads";

function previewSrc(u: UploadEntry): string | null {
  if (u.previewUrl) return u.previewUrl;
  if (u.attachmentId !== null && (u.kind === "image" || u.kind === "video")) {
    return `decibell-attachment://attach/${encodeURIComponent(u.serverId)}/${u.attachmentId}?variant=thumb&size=320`;
  }
  return null;
}

export default function UploadRow({ u, onNavigate }: { u: UploadEntry; onNavigate: () => void }) {
  const pause: RowAction = { label: "Pause", icon: <PauseIcon />, onClick: () => pauseUpload(u.pendingId, true) };
  const resume: RowAction = { label: "Resume", icon: <PlayIcon />, onClick: () => pauseUpload(u.pendingId, false) };
  const cancel: RowAction = { label: "Cancel", icon: <CloseIcon />, onClick: () => cancelUpload(u.pendingId) };
  const remove: RowAction = {
    label: "Remove from list",
    icon: <CloseIcon />,
    onClick: () => useTransfersStore.getState().removeUpload(u.pendingId),
  };
  const where = u.channelName ? `#${u.channelName}` : u.serverName;
  const fraction = u.totalBytes > 0 ? u.transferredBytes / u.totalBytes : 0;
  const common = { preview: <TransferPreview key={previewSrc(u) ?? "-"} src={previewSrc(u)} kind={u.kind} />, name: u.filename };

  switch (u.state) {
    case "uploading":
      return (
        <TransferRow
          {...common}
          status={[
            formatProgress(u.transferredBytes, u.totalBytes),
            u.speedBps > 0 ? formatSpeed(u.speedBps) : "",
            where ? `to ${where}` : "",
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
          status={[`Paused · ${formatProgress(u.transferredBytes, u.totalBytes)}`, where ? `to ${where}` : ""]
            .filter(Boolean)
            .join(" · ")}
          progress={fraction}
          paused
          actions={[resume, cancel]}
          persistentActions
        />
      );
    case "done":
      return (
        <TransferRow
          {...common}
          status={[where ? `Uploaded to ${where}` : "Uploaded", formatFileSize(u.totalBytes)].join(" · ")}
          actions={[remove]}
          persistentActions={false}
          onActivate={() => {
            onNavigate();
            const messageId =
              u.attachmentId !== null ? messageIdForAttachment(u.serverId, u.channelId, u.attachmentId) : 0;
            goToMessage(u.serverId, u.channelId, messageId);
          }}
          activateLabel="Go to message"
        />
      );
    case "failed":
      return (
        <TransferRow
          {...common}
          status={`Failed — ${u.error ?? "upload failed"}`}
          error
          actions={[remove]}
          persistentActions
        />
      );
    case "cancelled":
      return <TransferRow {...common} struck status="Cancelled" actions={[remove]} persistentActions={false} />;
  }
}
