import { useUiStore } from "../stores/uiStore";
import ConfirmModal from "./ConfirmModal";

interface Props {
  /// `activeModal === "delete-message-confirm"` plus whatever the parent
  /// needs to know which message — see ConfirmModal for why the parent
  /// drives this instead of gating the mount.
  open: boolean;
  /// Fired when the user confirms the deletion. Caller is responsible
  /// for triggering the optimistic snapshot/remove + native command.
  /// This component just owns the copy.
  onConfirm: () => void;
}

/// Confirmation for per-message deletion. Rendered by each parent panel
/// (ChatPanel / DmChatPanel), which also tracks which message is being
/// deleted in its own local state. Chrome + keyboard (Esc / Enter) come
/// from ConfirmModal.
export default function DeleteMessageConfirmModal({ open, onConfirm }: Props) {
  const closeModal = useUiStore((s) => s.closeModal);
  return (
    <ConfirmModal
      open={open}
      title="Delete message"
      confirmLabel="Delete"
      onCancel={closeModal}
      onConfirm={() => {
        onConfirm();
        closeModal();
      }}
    >
      <p className="mt-2 text-[13px] text-text-secondary">
        Delete this message? This cannot be undone.
      </p>
      <p className="mt-3 text-[11px] text-text-muted">
        Tip: hold{" "}
        <kbd className="rounded-sm border border-border bg-bg-darkest px-1.5 py-0.5 font-mono text-[10px] text-text-secondary">
          Shift
        </kbd>{" "}
        while clicking the trash icon to skip this confirmation next time.
      </p>
    </ConfirmModal>
  );
}
