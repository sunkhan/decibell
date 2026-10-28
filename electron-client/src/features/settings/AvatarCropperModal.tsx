// Profile picture editor: the shared picture editor with a square frame in
// the theme's avatar shape and previews of where the picture shows.
//
// Save renders the crop at 256×256 (JPEG, components/pictureEditor) and
// ships it through upload_avatar → central checks the magic and the 1 MB cap,
// stores it, answers, and pushes AvatarChanged → avatarStore refetches →
// UserAvatar across the app re-renders.

import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { AVATAR_RADIUS } from "../../components/LetterAvatar";
import { StatusDot } from "../../components/StatusDot";
import { Crop, PictureEditor, PreviewLabel } from "../../components/pictureEditor/PictureEditor";
import { userGradient } from "../../utils/colors";
import { useOwnDisplayStatus } from "../../stores/presenceStore";

interface Props {
  file: File;
  /// The saved picture, shown in Settings until the store has fetched it.
  onSaved: (url: string) => void;
  onClose: () => void;
}

const OUTPUT = { w: 256, h: 256 };

export function AvatarCropperModal({ file, onSaved, onClose }: Props) {
  return (
    <PictureEditor
      file={file}
      kind="avatar"
      title="Edit profile picture"
      aspect={1}
      fit={false}
      radius={() => AVATAR_RADIUS}
      output={OUTPUT}
      target="avatar"
      previews={<AvatarPreviews />}
      onSave={async (jpeg) => {
        const result = (await invoke("upload_avatar", { jpeg })) as { success: boolean; message: string } | null;
        if (!result?.success) throw new Error(result?.message || "Couldn't save your picture.");
      }}
      onSaved={onSaved}
      onClose={onClose}
    />
  );
}

/// The picture where people meet it: your profile card, a message, the
/// member list. Same sizes, rings and status cut-outs as the real ones.
function AvatarPreviews() {
  const uid = useAuthStore((s) => s.uid);
  const name = useAuthStore((s) => s.username) ?? "";
  const status = useOwnDisplayStatus();
  return (
    <div className="flex flex-col gap-4">
      <div>
        <PreviewLabel>Profile</PreviewLabel>
        <div className="overflow-hidden rounded-lg border border-border bg-bg-light">
          <div className="h-12" style={{ background: userGradient(uid, name) }} />
          <div className="relative mx-3.5 h-7">
            <div className="absolute -top-8 left-0 flex bg-bg-light ring-4 ring-bg-light" style={{ borderRadius: AVATAR_RADIUS }}>
              <Crop width={64} height={64} radius={AVATAR_RADIUS} />
              <StatusDot status={status} ring="var(--color-bg-light)" ringWidth={3} className="absolute -bottom-px -right-px h-4 w-4" />
            </div>
          </div>
          <div className="truncate px-3.5 pb-3 pt-2 font-display text-[16px] font-semibold text-text-primary">{name}</div>
        </div>
      </div>
      <div>
        <PreviewLabel>Chat</PreviewLabel>
        <div className="flex gap-3 rounded-lg bg-bg-mid px-3 py-2.5">
          <Crop width={40} height={40} radius={AVATAR_RADIUS} />
          <div className="min-w-0">
            <div className="flex items-baseline gap-2">
              <span className="truncate font-channel text-sender font-emphasis text-text-primary">{name}</span>
              <span className="shrink-0 font-meta text-meta tabular-nums text-text-muted">18:24</span>
            </div>
            <div className="text-body leading-body text-text-primary">new picture, who dis</div>
          </div>
        </div>
      </div>
      <div>
        <PreviewLabel>Member list</PreviewLabel>
        <div className="flex h-11 items-center gap-2.5 rounded-lg bg-bg-tertiary px-2.5">
          <div className="relative flex">
            <Crop width={34} height={34} radius={AVATAR_RADIUS} />
            <StatusDot status={status} ring="var(--color-bg-tertiary)" className="avatar-dot absolute -bottom-px -right-px" />
          </div>
          <span className="min-w-0 truncate font-channel text-member font-medium text-text-secondary">{name}</span>
        </div>
      </div>
    </div>
  );
}
