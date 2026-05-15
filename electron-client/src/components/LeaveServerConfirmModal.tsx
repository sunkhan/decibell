import { useEffect } from "react";
import { useUiStore } from "../stores/uiStore";

interface Props {
  /// Name of the server the user is about to leave — shown verbatim
  /// in the modal copy so the user has no ambiguity about which
  /// server is at stake.
  serverName: string;
  /// Fired when the user confirms. Caller is responsible for the
  /// actual leave_server invoke + local store cleanup.
  onConfirm: () => void;
}

/// Confirmation modal for leaving a community server. Mounted by
/// ServerChannelsSidebar based on useUiStore.activeModal ===
/// "leave-server-confirm". The parent tracks which server is being
/// left in its own local state and passes the name + onConfirm here.
///
/// Keyboard: Esc cancels, Enter confirms. Mirrors
/// DeleteMessageConfirmModal in chrome + interaction.
export default function LeaveServerConfirmModal({ serverName, onConfirm }: Props) {
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
          Leave server
        </h2>
        <p className="mt-2 text-[13px] text-text-secondary">
          Leave <span className="font-semibold text-text-primary">{serverName}</span>?
          You will need a new invite to rejoin.
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
            Leave
          </button>
        </div>
      </div>
    </div>
  );
}
