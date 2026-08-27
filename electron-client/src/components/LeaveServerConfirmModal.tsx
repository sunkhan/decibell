import { useUiStore } from "../stores/uiStore";
import ConfirmModal from "./ConfirmModal";

interface Props {
  /// `activeModal === "leave-server-confirm"` plus a known target — see
  /// ConfirmModal for why the parent drives this instead of gating the
  /// mount.
  open: boolean;
  /// Name of the server the user is about to leave — shown verbatim
  /// in the copy so there is no ambiguity about which server is at
  /// stake.
  serverName: string;
  /// Fired when the user confirms. Caller is responsible for the
  /// actual leave_server invoke + local store cleanup.
  onConfirm: () => void;
}

/// Confirmation for leaving a community server. Rendered by
/// ServerChannelsSidebar, which tracks which server is being left in
/// its own local state. Chrome + keyboard (Esc / Enter) come from
/// ConfirmModal.
export default function LeaveServerConfirmModal({ open, serverName, onConfirm }: Props) {
  const closeModal = useUiStore((s) => s.closeModal);
  return (
    <ConfirmModal
      open={open}
      title="Leave server"
      confirmLabel="Leave"
      onCancel={closeModal}
      onConfirm={() => {
        onConfirm();
        closeModal();
      }}
    >
      <p className="mt-2 text-[13px] text-text-secondary">
        Leave <span className="font-semibold text-text-primary">{serverName}</span>?
        You will need a new invite to rejoin.
      </p>
    </ConfirmModal>
  );
}
