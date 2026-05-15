import { useEffect } from "react";
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
///
/// Keyboard: Esc cancels, Enter confirms. Both shortcuts capture on
/// the document so they work even when no field inside the modal is
/// focused.
export default function DeleteMessageConfirmModal({ onConfirm }: Props) {
  const closeModal = useUiStore((s) => s.closeModal);

  const handleConfirm = () => {
    onConfirm();
    closeModal();
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeModal();
      } else if (e.key === "Enter") {
        e.preventDefault();
        handleConfirm();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
    // handleConfirm closes over onConfirm/closeModal — both stable
    // references via Zustand, so the empty dep array is fine.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
