import { useEffect, useRef, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { clampLine } from "../../../lib/utf8";
import { useChatStore } from "../../../stores/chatStore";
import { useAuthStore } from "../../../stores/authStore";
import { toast } from "../../../stores/toastStore";
import { ServerPictureCropperModal } from "../ServerPictureCropperModal";
import ConfirmModal from "../../../components/ConfirmModal";
import { DropVeil, PictureButton, usePictureDrop } from "../../../components/pictureEditor/PictureButton";
import { ImageIcon } from "../../../components/pictureEditor/icons";
import { PERM, usePermission } from "../permissions";
import Switch from "../../../components/Switch";
import { SELECT_TRIGGER_CLASS, SelectChevron } from "../../../components/Select";
import { UserAvatar } from "../../../components/UserAvatar";
import { useEchoSave } from "../saveEcho";
import { useLeaveGuard } from "../leaveGuard";
import { Popover, SaveBar, SearchField, anchorBelow } from "./settingsUi";
import { useMemberSearch } from "./useMemberSearch";
import type { MenuAnchor } from "../../../hooks/useMenuPosition";
import type { ServerMember } from "../../../types";
import { SERVER_DESCRIPTION_MAX_BYTES, SERVER_NAME_MAX_BYTES } from "../limits";
import { useUserName, useUsersStore, type Uid } from "../../../stores/usersStore";
import { useMember } from "../../../hooks/useDisplayName";

type Meta = { name: string; description: string };

/// The preview is the tile at 2×: its corner too.
const PREVIEW_RADIUS = "calc(var(--radius-md) * 2)";

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
  const ownUid = useAuthStore((s) => s.uid);
  const isOwner = !!ownUid && !!owner && owner === ownUid;
  // The owner's row, looked up if the roster hasn't paged that far.
  const ownerName = useUserName(owner, useMember(serverId, owner ?? 0)?.username ?? "Unknown user");
  const names = useUsersStore((s) => s.names);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [cropperFile, setCropperFile] = useState<File | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  // Any picture: the editor crops it to the tile and the export always fits
  // the server's 1 MB cap, so there is nothing to check here.
  const drop = usePictureDrop(setCropperFile, canManageServer);
  // The picture just saved, shown until the tile's copy arrives.
  const [justSaved, setJustSaved] = useState<string | null>(null);
  const savedFrom = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (justSaved && pictureDataUrl && pictureDataUrl !== savedFrom.current) setJustSaved(null);
  }, [justSaved, pictureDataUrl]);
  useEffect(() => () => {
    if (justSaved) URL.revokeObjectURL(justSaved);
  }, [justSaved]);

  const serverName = meta?.name ?? server?.name ?? "Server";
  const serverDescription = meta?.description ?? server?.description ?? "";

  // Identity editor (MANAGE_SERVER). The draft follows the server until
  // edited, so someone else's change shows and never drops your typing.
  // After Save it stays until the server's meta push shows what was sent
  // (saveEcho.ts).
  const [draft, setDraft] = useState<Meta | null>(null);
  useEffect(() => setDraft(null), [serverId]);
  const nameDraft = draft?.name ?? serverName;
  const descDraft = draft?.description ?? serverDescription;
  const editMeta = (patch: Partial<Meta>) => setDraft({ name: nameDraft, description: descDraft, ...patch });
  const { saving: savingMeta, start: startMeta, stop: stopMeta } = useEchoSave<Meta>(
    (sent) => serverName === sent.name && serverDescription === sent.description,
    // Edits made while the save was out stay.
    (sent) => setDraft((d) => (d && (d.name.trim() !== sent.name || d.description.trim() !== sent.description) ? d : null)),
  );
  const metaDirty =
    canManageServer && (nameDraft.trim() !== serverName || descDraft.trim() !== serverDescription);
  // Closing the window or changing tab shakes the save bar (leaveGuard.ts).
  const leaveNudge = useLeaveGuard(metaDirty);
  const saveMeta = async () => {
    if (!canManageServer || !metaDirty || !nameDraft.trim() || savingMeta) return;
    const sent = { name: nameDraft.trim(), description: descDraft.trim() };
    startMeta(sent);
    try {
      await invoke("update_server", { serverId, ...sent });
    } catch (err) {
      stopMeta();
      toast.error("Couldn't update server", String(err));
    }
  };

  // Public listing (MANAGE_SERVER): discovery + invite-less join. Reflected
  // from the store, which the server_meta_updated broadcast keeps live; a
  // flip shows at once and holds until that push confirms it.
  const publicListing = useChatStore((s) => s.serverPublicListing[serverId] ?? false);
  const {
    saving: savingPublic,
    sent: sentPublic,
    start: startPublic,
    stop: stopPublic,
  } = useEchoSave<boolean>(
    (sent) => publicListing === sent,
    () => {},
  );
  const togglePublic = async () => {
    if (!canManageServer || savingPublic) return;
    startPublic(!publicListing);
    try {
      // Send the current authoritative name/description so flipping visibility
      // never commits an unsaved rename or blanks the name.
      await invoke("update_server", {
        serverId,
        name: serverName,
        description: serverDescription,
        publicListing: !publicListing,
      });
    } catch (err) {
      stopPublic();
      toast.error("Couldn't update visibility", String(err));
    }
  };

  // A refusal ends both waits. Always listening, so a quick reply can't
  // beat the listener.
  useEffect(() => {
    const un = listen<{ serverId: string; success: boolean; message: string }>(
      "server_update_responded",
      (event) => {
        if (event.payload.serverId !== serverId || event.payload.success) return;
        stopMeta();
        stopPublic();
        toast.error("Couldn't update server", event.payload.message || "Unknown error.");
      },
    );
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId, stopMeta, stopPublic]);

  // Ownership transfer (owner only, typed confirmation). The new owner is
  // found by searching the server (OwnerPicker), not among the members the
  // roster happens to hold; the pick is kept as its record, since search
  // results come and go.
  const [transferTarget, setTransferTarget] = useState<ServerMember | null>(null);
  const [transferConfirm, setTransferConfirm] = useState("");
  const [transferring, setTransferring] = useState(false);
  const [ownerPicker, setOwnerPicker] = useState<MenuAnchor | null>(null);
  const ownerPickerRef = useRef<HTMLButtonElement>(null);
  const nameFor = (m: { uid: Uid; username: string }) => names.get(m.uid) ?? m.username;
  const targetName = transferTarget ? nameFor(transferTarget) : "";
  const runTransfer = async () => {
    if (!isOwner || !transferTarget || !targetName || transferConfirm !== targetName) return;
    setTransferring(true);
    try {
      await invoke("transfer_ownership", { serverId, newOwner: targetName, newOwnerUid: transferTarget.uid });
      setTransferTarget(null);
      setTransferConfirm("");
    } catch (err) {
      toast.error("Transfer failed", String(err));
    } finally {
      setTransferring(false);
    }
  };

  const removePicture = () => {
    setConfirmRemove(false);
    invoke("update_server_picture", {
      serverId,
      data: new Uint8Array(0),
    }).catch((err) => {
      console.error("update_server_picture:", err);
      toast.error("Failed to remove", "Please try again.");
    });
  };

  return (
    <div>
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
                    onChange={(e) => editMeta({ name: clampLine(e.target.value, SERVER_NAME_MAX_BYTES) })}
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
                    onChange={(e) => editMeta({ description: clampLine(e.target.value, SERVER_DESCRIPTION_MAX_BYTES) })}
                    className="w-full resize-none rounded-md border border-border bg-bg-lighter px-3 py-2 text-[13px] leading-[1.5] text-text-secondary outline-none transition-all focus:border-accent focus:shadow-ring"
                  />
                </div>
                <div className="mt-1 flex items-start justify-between gap-3 border-t border-border-divider pt-3">
                  <div className="min-w-0">
                    <div className="text-[13px] font-medium text-text-primary">Public listing</div>
                    <div className="mt-0.5 text-[12px] leading-[1.45] text-text-muted">
                      List this server in Discover so anyone can find and join it without an
                      invite. Off keeps it hidden and invite-only.
                    </div>
                  </div>
                  <Switch
                    checked={sentPublic ?? publicListing}
                    onToggle={togglePublic}
                    disabled={savingPublic}
                    label="Public listing"
                  />
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
                <div className="mt-0.5 text-[13px] text-text-secondary">{ownerName}</div>
              </div>
            )}
          </div>
          {!canManageServer && (
            <p className="text-[12px] leading-[1.5] text-text-muted">
              Members with Manage Server can change the name and description.
            </p>
          )}
        </div>

        {/* Picture. The preview is the tile's shape at twice its size; click
            it or drop a picture on the card. */}
        <div className="flex flex-col gap-2">
          <h3 className="text-[14px] font-semibold text-text-primary">Server picture</h3>
          <div
            {...drop.handlers}
            className="relative flex items-center gap-5 rounded-md border border-border-divider bg-bg-light p-3.5"
          >
            <PictureButton
              onPick={setCropperFile}
              radius={PREVIEW_RADIUS}
              label={hasPicture ? "Change picture" : "Upload picture"}
              target="server-picture"
              disabled={!canManageServer}
              inputRef={fileInputRef}
            >
              {justSaved || (hasPicture && pictureDataUrl) ? (
                <img
                  src={justSaved ?? pictureDataUrl}
                  alt={serverName}
                  className="h-22 w-64 object-cover"
                  style={{ borderRadius: PREVIEW_RADIUS }}
                />
              ) : (
                <div
                  className="flex h-22 w-64 flex-col items-center justify-center gap-1.5 border-2 border-dashed border-border text-[12px] font-medium text-text-muted"
                  style={{ borderRadius: PREVIEW_RADIUS }}
                >
                  <ImageIcon size={18} />
                  {canManageServer ? "Drop a picture or click" : "No picture"}
                </div>
              )}
            </PictureButton>
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-medium text-text-primary">
                On the server bar, invite cards and Discover
              </div>
              <div className="mt-0.5 text-[12px] leading-[1.45] text-text-muted">
                {canManageServer
                  ? "Any JPEG, PNG, WebP or GIF. You'll crop it to the bar's shape next."
                  : "Members with Manage Server can change it."}
              </div>
              {canManageServer && (
                <div className="mt-3 flex gap-2">
                  <button
                    onClick={() => fileInputRef.current?.click()}
                    className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
                  >
                    {hasPicture ? "Change picture" : "Upload picture"}
                  </button>
                  {hasPicture && (
                    <button
                      onClick={() => setConfirmRemove(true)}
                      className="rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                    >
                      Remove
                    </button>
                  )}
                </div>
              )}
            </div>
            {drop.over && <DropVeil label="Drop to use as the server picture" />}
          </div>
        </div>

        {isOwner && (
          <div className="flex flex-col gap-3">
            <h3 className="text-[14px] font-semibold text-error">Transfer ownership</h3>
            <div className="rounded-md border border-error/25 bg-error/5 p-3.5">
              <p className="text-[12px] leading-[1.55] text-text-muted">
                The new owner gets every permission and outranks every role; you
                keep only the roles you hold. This can't be undone by you.
              </p>
              <button
                ref={ownerPickerRef}
                type="button"
                aria-label="New owner"
                aria-haspopup="menu"
                aria-expanded={!!ownerPicker}
                onClick={() => setOwnerPicker(ownerPicker ? null : anchorBelow(ownerPickerRef.current!))}
                onKeyDown={(e) => {
                  // As a Select opens on ArrowDown.
                  if (e.key !== "ArrowDown" || ownerPicker) return;
                  e.preventDefault();
                  setOwnerPicker(anchorBelow(e.currentTarget));
                }}
                className={`mt-3 ${SELECT_TRIGGER_CLASS}`}
              >
                {transferTarget && <UserAvatar uid={transferTarget.uid} name={targetName} size={18} />}
                <span className={`min-w-0 flex-1 truncate ${transferTarget ? "" : "text-text-muted"}`}>
                  {transferTarget
                    ? transferTarget.nickname
                      ? `${transferTarget.nickname} (${targetName})`
                      : targetName
                    : "Choose a member…"}
                </span>
                <SelectChevron />
              </button>
              {ownerPicker && (
                <OwnerPicker
                  serverId={serverId}
                  ownUid={ownUid}
                  anchor={ownerPicker}
                  ignore={ownerPickerRef}
                  onClose={() => {
                    setOwnerPicker(null);
                    ownerPickerRef.current?.focus();
                  }}
                  onPick={(m) => {
                    setOwnerPicker(null);
                    setTransferTarget(m);
                    setTransferConfirm("");
                    ownerPickerRef.current?.focus();
                  }}
                />
              )}
              {targetName && (
                <>
                  <input
                    value={transferConfirm}
                    onChange={(e) => setTransferConfirm(e.target.value)}
                    placeholder={`Type ${targetName} to confirm`}
                    className="mt-2 w-full rounded-md border border-border bg-bg-lighter px-3 py-2 text-[13px] text-text-primary outline-none transition-all focus:border-error"
                  />
                  <button
                    onClick={runTransfer}
                    disabled={transferConfirm !== targetName || transferring}
                    className="mt-2 w-full rounded-md bg-error px-4 py-2 text-[13px] font-medium text-on-accent transition-colors hover:bg-error/90 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {transferring ? "Transferring…" : `Transfer ownership to ${targetName}`}
                  </button>
                </>
              )}
            </div>
          </div>
        )}

        {cropperFile && (
          <ServerPictureCropperModal
            serverId={serverId}
            file={cropperFile}
            onSaved={(url) => {
              savedFrom.current = pictureDataUrl;
              setJustSaved(url);
            }}
            onClose={() => setCropperFile(null)}
          />
        )}
        <ConfirmModal
          open={confirmRemove}
          title="Remove the server picture?"
          confirmLabel="Remove"
          onConfirm={removePicture}
          onCancel={() => setConfirmRemove(false)}
        >
          <p className="mt-2 text-[13px] leading-[1.55] text-text-secondary">
            Members will see the server's letter and colour on the server bar again.
          </p>
        </ConfirmModal>
      </div>
      {canManageServer && (
        <SaveBar
          show={metaDirty}
          nudge={leaveNudge}
          saving={savingMeta}
          onReset={() => setDraft(null)}
          onSave={saveMeta}
          saveDisabled={!nameDraft.trim()}
        />
      )}
    </div>
  );
}

/// The new owner, searched for on the server (MEMBER_SEARCH_REQ): every
/// member, whether the roster has paged them in or not. The results live
/// here only, never in the roster store. Accounts only (a community
/// placeholder can't own the server), and not yourself.
function OwnerPicker({
  serverId,
  ownUid,
  anchor,
  ignore,
  onClose,
  onPick,
}: {
  serverId: string;
  ownUid: Uid;
  anchor: MenuAnchor;
  ignore: React.RefObject<HTMLElement | null>;
  onClose: () => void;
  onPick: (member: ServerMember) => void;
}) {
  const [query, setQuery] = useState("");
  const found = useMemberSearch(serverId, query, 0);
  const names = useUsersStore((s) => s.names);
  const nameFor = (m: ServerMember) => names.get(m.uid) ?? m.username;
  const shown = found.members.filter((m) => m.uid > 0 && m.uid !== ownUid);
  return (
    <Popover anchor={anchor} onClose={onClose} ignore={ignore} className="flex w-80 flex-col p-2">
      <SearchField value={query} onChange={setQuery} placeholder="Find a member" autoFocus />
      {/* A fixed height: the popover sits low in the tab and may open above
          the trigger, and a list that grew and shrank as results came in
          would flip it from side to side while typing. */}
      <div className="mt-2 flex h-64 flex-col overflow-y-auto scrollbar-thin">
        {shown.map((m) => (
          <button
            key={m.uid}
            type="button"
            role="menuitem"
            onClick={() => onPick(m)}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left outline-none hover:bg-surface-hover focus-visible:bg-surface-hover"
          >
            <UserAvatar uid={m.uid} name={nameFor(m)} size={24} />
            <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-text-primary">
              {m.nickname || nameFor(m)}
              {m.nickname && <span className="ml-1.5 font-meta text-meta font-normal text-text-muted">{nameFor(m)}</span>}
            </span>
          </button>
        ))}
        {found.hasMore && (
          <button
            type="button"
            onClick={found.loadMore}
            disabled={found.loading}
            className="rounded-md px-2 py-1.5 text-left text-[12px] font-medium text-text-muted hover:bg-surface-hover hover:text-text-primary disabled:opacity-60"
          >
            {found.loading ? "Loading…" : "Show more"}
          </button>
        )}
        {!found.loaded && <p className="px-2 py-4 text-center text-[12px] text-text-muted">Searching…</p>}
        {found.loaded && found.error && (
          <p className="px-2 py-4 text-center text-[12px] leading-[1.5] text-error">{found.error}</p>
        )}
        {found.loaded && !found.error && shown.length === 0 && !found.hasMore && (
          <p className="px-2 py-4 text-center text-[12px] text-text-muted">
            {query.trim() ? "No one else matches." : "No one else is a member yet."}
          </p>
        )}
      </div>
    </Popover>
  );
}
