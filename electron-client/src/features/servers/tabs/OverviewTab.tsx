import { useRef, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { toast } from "../../../stores/toastStore";
import { stringToGradient } from "../../../utils/colors";
import { ServerPictureCropperModal } from "../ServerPictureCropperModal";
import { PERM, usePermission } from "../permissions";

const MAX_BYTES = 1024 * 1024;

/// Overview: server identity (name/description, read-only — they come
/// from the operator's env vars) + picture management (MANAGE_SERVER).
export default function OverviewTab({ serverId }: { serverId: string }) {
  const server = useChatStore((s) => s.servers.find((x) => x.id === serverId));
  const meta = useChatStore((s) => s.serverMeta[serverId]);
  const owner = useChatStore((s) => s.serverOwner[serverId]);
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[serverId] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[serverId]);
  const canManageServer = usePermission(serverId, PERM.MANAGE_SERVER);
  const hasPicture = pictureVersion !== "";

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [cropperFile, setCropperFile] = useState<File | null>(null);

  const serverName = meta?.name ?? server?.name ?? "Server";
  const serverDescription = meta?.description ?? server?.description ?? "";

  const onFileSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-select of the same file
    if (!file) return;
    if (file.size > MAX_BYTES) {
      toast.error("Image too large", "Maximum size is 1 MB.");
      return;
    }
    // Sniff JPEG/PNG magic bytes.
    const head = new Uint8Array(await file.slice(0, 4).arrayBuffer());
    const isPng =
      head[0] === 0x89 && head[1] === 0x50 && head[2] === 0x4e && head[3] === 0x47;
    const isJpeg = head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff;
    if (!isPng && !isJpeg) {
      toast.error("Unsupported format", "Only JPEG and PNG are supported.");
      return;
    }
    setCropperFile(file);
  };

  const onRemove = () => {
    if (
      !window.confirm(
        "Remove the server picture? The default gradient and letter will be used instead.",
      )
    ) {
      return;
    }
    invoke("update_server_picture", {
      serverId,
      data: new Uint8Array(0),
    }).catch((err) => {
      console.error("update_server_picture:", err);
      toast.error("Failed to remove", "Please try again.");
    });
  };

  return (
    <div className="flex flex-col gap-6">
      {/* Identity */}
      <div className="flex flex-col gap-2">
        <h3 className="text-[14px] font-semibold text-text-primary">
          Server identity
        </h3>
        <div className="flex flex-col gap-2 rounded-md border border-border-divider bg-bg-light p-3.5">
          <div>
            <div className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
              Name
            </div>
            <div className="mt-0.5 text-[14px] font-medium text-text-primary">
              {serverName}
            </div>
          </div>
          {serverDescription && (
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                Description
              </div>
              <div className="mt-0.5 text-[13px] leading-[1.5] text-text-secondary">
                {serverDescription}
              </div>
            </div>
          )}
          {owner && (
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                Owner
              </div>
              <div className="mt-0.5 text-[13px] text-text-secondary">{owner}</div>
            </div>
          )}
        </div>
        <p className="text-[12px] leading-[1.5] text-text-muted">
          Name and description are set by the server operator's configuration
          and refresh on the server's next restart.
        </p>
      </div>

      {/* Picture */}
      <div className="flex flex-col gap-3">
        <h3 className="text-[14px] font-semibold text-text-primary">
          Server picture
        </h3>
        <p className="text-[13px] text-text-secondary">
          Shown in the server bar in place of the default gradient and letter.
          Square images work best; JPEG or PNG, max 1 MB.
        </p>
        <div className="flex items-center gap-6">
          {hasPicture && pictureDataUrl ? (
            <img
              src={pictureDataUrl}
              alt={serverName}
              className="h-[120px] w-[120px] rounded-lg object-cover"
            />
          ) : (
            <div
              className="flex h-[120px] w-[120px] items-center justify-center rounded-lg text-[44px] font-semibold text-white"
              style={{ background: stringToGradient(serverName) }}
            >
              {serverName.charAt(0).toUpperCase()}
            </div>
          )}
          {canManageServer ? (
            <div className="flex flex-col gap-2">
              <button
                onClick={() => fileInputRef.current?.click()}
                className="rounded-md bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-hover"
              >
                Upload picture
              </button>
              {hasPicture && (
                <button
                  onClick={onRemove}
                  className="rounded-md border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
                >
                  Remove picture
                </button>
              )}
              <input
                ref={fileInputRef}
                type="file"
                accept="image/jpeg,image/png"
                className="hidden"
                onChange={onFileSelected}
              />
            </div>
          ) : (
            <p className="max-w-[280px] text-[12px] leading-[1.5] text-text-muted">
              You need the Manage Server permission to change the picture.
            </p>
          )}
        </div>
      </div>

      {cropperFile && (
        <ServerPictureCropperModal
          serverId={serverId}
          file={cropperFile}
          onSave={() => setCropperFile(null)}
          onCancel={() => setCropperFile(null)}
        />
      )}
    </div>
  );
}
