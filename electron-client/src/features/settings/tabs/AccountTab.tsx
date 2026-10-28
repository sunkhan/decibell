import { useEffect, useRef, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { endCall } from "../../../features/call/callActions";
import { useAuthStore } from "../../../stores/authStore";
import { useAvatarStore } from "../../../stores/avatarStore";
import { UserAvatar } from "../../../components/UserAvatar";
import { AvatarCropperModal } from "../AvatarCropperModal";
import { AVATAR_RADIUS } from "../../../components/LetterAvatar";
import { DropVeil, PictureButton, usePictureDrop } from "../../../components/pictureEditor/PictureButton";
import { userGradient } from "../../../utils/colors";
import RenameUsernameModal from "../RenameUsernameModal";
import ChangePasswordModal from "../ChangePasswordModal";

export default function AccountTab() {
  const username = useAuthStore((s) => s.username);
  const selfUid = useAuthStore((s) => s.uid);
  const renames = useAuthStore((s) => s.renames);
  const devices = useAuthStore((s) => s.devices);
  const [renaming, setRenaming] = useState(false);
  const [changingPassword, setChangingPassword] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [pickedFile, setPickedFile] = useState<File | null>(null);
  const [removing, setRemoving] = useState(false);

  const handleLogout = async () => {
    try {
      // Hang up (and tell the peer) before the central session goes away.
      await endCall(null);
      await invoke("logout");
    } catch (err) {
      console.error("Logout failed:", err);
    }
  };

  const handlePickClick = () => fileInputRef.current?.click();
  // Drop a picture anywhere on the card.
  const drop = usePictureDrop(setPickedFile);

  const handleRemove = async () => {
    if (!username) return;
    setRemoving(true);
    try {
      // Server treats a zero-byte upload as "remove" — see main.cpp
      // UPDATE_AVATAR_REQ handler. The renderer's avatar will be
      // updated via the avatar_changed event (native, on success) → setVersion('') →
      // fetch returns missing → LetterAvatar.
      const result = (await invoke("upload_avatar", {
        jpeg: new Uint8Array(0),
      })) as { success: boolean; message: string; version: string };
      if (!result.success) {
        console.error("Avatar removal failed:", result.message);
      }
    } catch (err) {
      console.error("Avatar removal failed:", err);
    } finally {
      setRemoving(false);
    }
  };

  // Subscribe so the preview re-renders the moment the broadcast +
  // re-fetch lands.
  const entry = useAvatarStore((s) => s.entries.get(selfUid));
  const hasAvatar = entry?.status === "loaded";

  // The picture just saved, shown until the store has fetched it (the
  // editor's flight lands on it), so the old one never flashes back.
  const [justSaved, setJustSaved] = useState<{ url: string; version: string | undefined } | null>(null);
  useEffect(() => {
    if (!justSaved) return;
    const fetched = entry?.status === "loaded" && entry.version !== justSaved.version;
    const t = window.setTimeout(() => setJustSaved(null), fetched ? 0 : 10_000);
    return () => window.clearTimeout(t);
  }, [justSaved, entry?.status, entry?.version]);
  useEffect(() => () => {
    if (justSaved) URL.revokeObjectURL(justSaved.url);
  }, [justSaved]);

  return (
    <div>
      {/* Profile card: you as the profile popup shows you (banner, picture,
          name). The picture is the way to change it: click it, or drop a
          picture anywhere on the card. */}
      <div
        {...drop.handlers}
        className="relative overflow-hidden rounded-lg border border-border-divider bg-bg-light"
      >
        <div className="h-20" style={{ background: userGradient(selfUid, username ?? "") }} />
        <div className="flex items-end gap-4 px-5 pb-4">
          <div className="-mt-10 flex shrink-0 bg-bg-light ring-4 ring-bg-light" style={{ borderRadius: AVATAR_RADIUS }}>
            {username ? (
              <PictureButton
                onPick={setPickedFile}
                radius={AVATAR_RADIUS}
                label="Change picture"
                target="avatar"
                badge
                inputRef={fileInputRef}
              >
                {justSaved ? (
                  <img src={justSaved.url} alt="" className="size-20 object-cover" style={{ borderRadius: AVATAR_RADIUS }} />
                ) : (
                  <UserAvatar uid={selfUid} name={username} size={80} />
                )}
              </PictureButton>
            ) : (
              <div className="size-20 bg-bg-darkest" style={{ borderRadius: AVATAR_RADIUS }} />
            )}
          </div>
          <div className="min-w-0 flex-1 pt-3">
            <div className="truncate font-display text-[18px] font-semibold text-text-primary">{username}</div>
            <div className="mt-0.5 flex items-center gap-2 text-[12px] text-text-muted">
              <span>Logged in</span>
              {renames && <span aria-hidden>·</span>}
              {renames && (
                <button
                  onClick={() => setRenaming(true)}
                  className="text-text-secondary underline-offset-2 hover:text-text-primary hover:underline"
                >
                  Change username
                </button>
              )}
              {devices && <span aria-hidden>·</span>}
              {devices && (
                <button
                  onClick={() => setChangingPassword(true)}
                  className="text-text-secondary underline-offset-2 hover:text-text-primary hover:underline"
                >
                  Change password
                </button>
              )}
            </div>
          </div>
          <div className="flex shrink-0 gap-2">
            {hasAvatar && (
              <button
                onClick={handleRemove}
                disabled={removing}
                className="rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
              >
                {removing ? "Removing…" : "Remove"}
              </button>
            )}
            <button
              onClick={handlePickClick}
              className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
            >
              {hasAvatar ? "Change picture" : "Add a picture"}
            </button>
          </div>
        </div>
        {drop.over && <DropVeil label="Drop to use as your picture" />}
      </div>

      <RenameUsernameModal open={renaming} onClose={() => setRenaming(false)} />
      <ChangePasswordModal open={changingPassword} onClose={() => setChangingPassword(false)} />

      {pickedFile && (
        <AvatarCropperModal
          file={pickedFile}
          onSaved={(url) => setJustSaved({ url, version: entry?.version })}
          onClose={() => setPickedFile(null)}
        />
      )}

      {/* Logout */}
      <button
        onClick={handleLogout}
        className="mt-5 rounded-md border border-error/20 bg-error/10 px-5 py-2.5 text-[13px] font-medium text-error transition-colors hover:bg-error/20"
      >
        Log Out
      </button>
    </div>
  );
}
