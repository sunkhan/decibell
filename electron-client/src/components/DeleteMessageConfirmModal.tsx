import { useUiStore } from "../stores/uiStore";

interface Props {
  /// Fired when the user confirms the deletion. Caller is responsible
  /// for triggering the optimistic snapshot/remove + native command.
  /// This component just owns the styled confirmation UI.
  onConfirm: () => void;
}

/// Confirmation modal for per-message deletion. Rendered conditionally
/// by each parent panel (ChatPanel / DmChatPanel) based on
/// useUiStore.activeModal === "delete-message-confirm" — the parent
/// also tracks which message is being deleted in its own local state.
export default function DeleteMessageConfirmModal({ onConfirm }: Props) {
  const closeModal = useUiStore((s) => s.closeModal);

  const handleConfirm = () => {
    onConfirm();
    closeModal();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={closeModal}
    >
      <div
        className="w-full max-w-[400px] rounded-2xl border border-border bg-bg-secondary p-6 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-[16px] font-semibold text-text-primary">
          Delete message
        </h2>
        <p className="mt-2 text-[13px] text-text-secondary">
          Delete this message? This cannot be undone.
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <button
            onClick={closeModal}
            className="rounded-lg border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          <button
            onClick={handleConfirm}
            className="rounded-lg bg-error px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-error/90"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  );
}
