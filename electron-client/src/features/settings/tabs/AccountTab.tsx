import { useRef, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { endCall } from "../../../features/call/callActions";
import { useAuthStore } from "../../../stores/authStore";
import { useAvatarStore } from "../../../stores/avatarStore";
import { UserAvatar } from "../../../components/UserAvatar";
import { AvatarCropperModal } from "../AvatarCropperModal";
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

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    if (f) setPickedFile(f);
    // Reset the input so picking the same file twice still fires onChange.
    e.target.value = "";
  };

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

  const handleCropperSave = () => {
    setPickedFile(null);
    // The avatar_changed event (native, on success) drives the store update;
    // nothing to do here besides closing the modal.
  };

  // Subscribe so the preview re-renders the moment the broadcast +
  // re-fetch lands.
  const entry = useAvatarStore((s) => s.entries.get(selfUid));
  const hasAvatar = entry?.status === "loaded";

  return (
    <div>
      {/* User card */}
      <div className="flex items-center gap-4 rounded-md border border-border-divider bg-bg-light px-5 py-4">
        {username ? (
          <UserAvatar uid={selfUid} name={username} size={64} />
        ) : (
          <div className="h-16 w-16 rounded-sm bg-bg-darkest" />
        )}
        <div className="flex-1">
          <div className="text-[14px] font-medium text-text-primary">{username}</div>
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
        <div className="flex flex-col gap-2">
          <button
            onClick={handlePickClick}
            className="rounded-sm border border-border bg-bg-darkest px-3 py-1.5 text-[12px] text-text-secondary hover:bg-bg-dark"
          >
            Change picture
          </button>
          <button
            onClick={handleRemove}
            disabled={removing || !hasAvatar}
            className="rounded-sm border border-border px-3 py-1.5 text-[12px] text-text-muted hover:text-text-secondary disabled:opacity-40"
          >
            {removing ? "Removing…" : "Remove"}
          </button>
        </div>
      </div>

      <input
        ref={fileInputRef}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif"
        onChange={handleFileChange}
        className="hidden"
      />

      <RenameUsernameModal open={renaming} onClose={() => setRenaming(false)} />
      <ChangePasswordModal open={changingPassword} onClose={() => setChangingPassword(false)} />

      {pickedFile && (
        <AvatarCropperModal
          file={pickedFile}
          onSave={handleCropperSave}
          onCancel={() => setPickedFile(null)}
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
