import ConfirmModal from "../../components/ConfirmModal";
import { useChatStore } from "../../stores/chatStore";
import { EMPTY_LIST } from "../../lib/empty";
import { PERM, useChannelPermission } from "../servers/permissions";

export interface CategorySyncPromptState {
  /// The full new order the drop produced (sent as-is on Sync / Keep).
  ids: string[];
  channelId: string;
  categoryId: string;
  open: boolean;
}

/// Asked when a channel is dropped into a category whose permissions its
/// channels can follow (permissions v3, categorySync on). The server never
/// changes what a channel allows on a move, so "Keep" is just the reorder;
/// "Sync" is the reorder plus an explicit sync with that category.
export default function CategorySyncPrompt({
  serverId,
  prompt,
  onSync,
  onKeep,
  onCancel,
}: {
  serverId: string;
  prompt: CategorySyncPromptState | null;
  onSync: () => void;
  onKeep: () => void;
  onCancel: () => void;
}) {
  const channels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  const channel = channels.find((c) => c.id === prompt?.channelId);
  const category = channels.find((c) => c.id === prompt?.categoryId);
  // Syncing rewrites the channel's overwrites: MANAGE_ROLES in it, as on
  // the server (which also refuses a sync that would change bits you lack).
  const canSync = useChannelPermission(serverId, prompt?.channelId ?? null, PERM.MANAGE_ROLES);
  const label = channel ? (channel.type === "text" ? `#${channel.name}` : channel.name) : "this channel";
  const categoryName = category?.name ?? "this category";

  return (
    <ConfirmModal
      open={!!prompt?.open}
      title={`Move ${label} into ${categoryName}?`}
      confirmLabel="Sync permissions"
      confirmTone="accent"
      confirmDisabled={!canSync}
      secondaryLabel="Keep permissions"
      onConfirm={onSync}
      onSecondary={onKeep}
      onCancel={onCancel}
    >
      <p className="mt-2 text-[13px] leading-[1.55] text-text-secondary">
        {categoryName} has permissions its channels can follow. Sync {label} to use them, or keep
        the permissions it has now.
      </p>
      {!canSync && (
        <p className="mt-2 text-[12px] leading-[1.55] text-text-muted">
          Syncing needs Manage Roles in {label}.
        </p>
      )}
    </ConfirmModal>
  );
}
