import { useEffect, useRef, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { useAuthStore } from "../../../stores/authStore";
import { toast } from "../../../stores/toastStore";
import { stringToGradient } from "../../../utils/colors";
import { ServerPictureCropperModal } from "../ServerPictureCropperModal";
import { PERM, usePermission } from "../permissions";

const MAX_BYTES = 1024 * 1024;

/// Overview: server identity (name/description — editable with
/// MANAGE_SERVER since the DB became the source of truth), picture
/// management (MANAGE_SERVER) and ownership transfer (owner only).
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
  const currentUser = useAuthStore((s) => s.username);
  const isOwner = !!currentUser && !!owner && owner === currentUser;
  const members = useChatStore((s) => s.membersByServer[serverId] ?? EMPTY_LIST);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [cropperFile, setCropperFile] = useState<File | null>(null);

  const serverName = meta?.name ?? server?.name ?? "Server";
  const serverDescription = meta?.description ?? server?.description ?? "";

  // Identity editor (MANAGE_SERVER). Drafts reset whenever the server
  // pushes new meta (our own save, or someone else's).
  const [nameDraft, setNameDraft] = useState(serverName);
  const [descDraft, setDescDraft] = useState(serverDescription);
  const [savingMeta, setSavingMeta] = useState(false);
  useEffect(() => {
    setNameDraft(serverName);
    setDescDraft(serverDescription);
  }, [serverName, serverDescription]);
  useEffect(() => {
    const un = listen<{ serverId: string; success: boolean; message: string }>(
      "server_update_responded",
      (event) => {
        if (event.payload.serverId !== serverId) return;
        setSavingMeta(false);
        if (!event.payload.success) {
          toast.error("Couldn't update server", event.payload.message || "Unknown error.");
        }
      },
    );
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId]);
  const metaDirty =
    nameDraft.trim() !== serverName || descDraft.trim() !== serverDescription;
  const saveMeta = async () => {
    if (!canManageServer || !metaDirty || !nameDraft.trim()) return;
    setSavingMeta(true);
    try {
      await invoke("update_server", {
        serverId,
        name: nameDraft.trim(),
        description: descDraft.trim(),
      });
    } catch (err) {
      setSavingMeta(false);
      toast.error("Couldn't update server", String(err));
    }
  };

  // Ownership transfer (owner only, typed confirmation).
  const [transferTarget, setTransferTarget] = useState("");
  const [transferConfirm, setTransferConfirm] = useState("");
  const [transferring, setTransferring] = useState(false);
  const transferCandidates = members.filter((m) => m.username !== currentUser);
  const runTransfer = async () => {
    if (!isOwner || !transferTarget || transferConfirm !== transferTarget) return;
    setTransferring(true);
    try {
      await invoke("transfer_ownership", { serverId, newOwner: transferTarget });
      setTransferTarget("");
      setTransferConfirm("");
    } catch (err) {
      toast.error("Transfer failed", String(err));
    } finally {
      setTransferring(false);
    }
  };

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
          {canManageServer ? (
            <>
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                  Name
                </div>
                <input
                  value={nameDraft}
                  maxLength={64}
                  onChange={(e) => setNameDraft(e.target.value)}
                  className="w-full rounded-md border border-border bg-bg-lighter px-3 py-2 text-[14px] font-medium text-text-primary outline-none transition-all focus:border-accent focus:shadow-ring"
                />
              </div>
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                  Description
                </div>
                <textarea
                  value={descDraft}
                  maxLength={512}
                  rows={2}
                  onChange={(e) => setDescDraft(e.target.value)}
                  className="w-full resize-none rounded-md border border-border bg-bg-lighter px-3 py-2 text-[13px] leading-[1.5] text-text-secondary outline-none transition-all focus:border-accent focus:shadow-ring"
                />
              </div>
              <div className="flex justify-end">
                <button
                  onClick={saveMeta}
                  disabled={!metaDirty || savingMeta || !nameDraft.trim()}
                  className="rounded-md bg-accent px-4 py-2 text-[13px] font-medium text-on-accent transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
                >
                  {savingMeta ? "Saving…" : "Save"}
                </button>
              </div>
            </>
          ) : (
            <>
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
            </>
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
        {!canManageServer && (
          <p className="text-[12px] leading-[1.5] text-text-muted">
            Members with Manage Server can change the name and description.
          </p>
        )}
      </div>

      {isOwner && (
        <div className="flex flex-col gap-3">
          <h3 className="text-[14px] font-semibold text-error">Transfer ownership</h3>
          <div className="rounded-md border border-error/25 bg-error/5 p-3.5">
            <p className="text-[12px] leading-[1.55] text-text-muted">
              The new owner gets every permission and outranks every role; you
              keep only the roles you hold. This can't be undone by you.
            </p>
            <select
              value={transferTarget}
              onChange={(e) => {
                setTransferTarget(e.target.value);
                setTransferConfirm("");
              }}
              className="mt-3 w-full appearance-none rounded-md border border-border bg-bg-lighter px-3 py-2.5 pr-9 text-[13px] text-text-primary outline-none transition-all hover:border-text-faint focus:border-accent focus:shadow-ring"
            >
              <option value="">Choose a member…</option>
              {transferCandidates.map((m) => (
                <option key={m.username} value={m.username}>
                  {m.nickname ? `${m.nickname} (${m.username})` : m.username}
                </option>
              ))}
            </select>
            {transferTarget && (
              <>
                <input
                  value={transferConfirm}
                  onChange={(e) => setTransferConfirm(e.target.value)}
                  placeholder={`Type ${transferTarget} to confirm`}
                  className="mt-2 w-full rounded-md border border-border bg-bg-lighter px-3 py-2 text-[13px] text-text-primary outline-none transition-all focus:border-error"
                />
                <button
                  onClick={runTransfer}
                  disabled={transferConfirm !== transferTarget || transferring}
                  className="mt-2 w-full rounded-md bg-error px-4 py-2 text-[13px] font-medium text-on-accent transition-colors hover:bg-error/90 disabled:cursor-not-allowed disabled:opacity-40"
                >
                  {transferring ? "Transferring…" : `Transfer ownership to ${transferTarget}`}
                </button>
              </>
            )}
          </div>
        </div>
      )}

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
