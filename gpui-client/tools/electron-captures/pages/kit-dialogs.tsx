// The GPUI kit's `kit-dialogs`: the renderer's ConfirmModal with DeleteMessageConfirmModal's body,
// drawn as the whole window (the dialog centres in it).
import ConfirmModal from "@app/components/ConfirmModal";
import { Framed } from "./_frame";

export const size = { width: 1280, height: 800 };

export default function KitDialogs() {
  return (
    <Framed>
      <div className="flex h-full w-full flex-col items-start gap-6 bg-bg-primary p-6">
        <div className="text-[13px] text-text-secondary">A message list would be under this dialog.</div>
        <button className="rounded-md bg-error px-4 py-2 text-[13px] font-semibold text-on-error transition-colors hover:bg-error/90">
          Delete message…
        </button>
      </div>
      <ConfirmModal open title="Delete message" confirmLabel="Delete" onCancel={() => {}} onConfirm={() => {}}>
        <p className="mt-2 text-[13px] text-text-secondary">Delete this message? This cannot be undone.</p>
        <p className="mt-3 text-[11px] text-text-muted">
          Tip: hold{" "}
          <kbd className="rounded-sm border border-border bg-bg-darkest px-1.5 py-0.5 font-mono text-[10px] text-text-secondary">Shift</kbd>{" "}
          while clicking the trash icon to skip this confirmation next time.
        </p>
      </ConfirmModal>
    </Framed>
  );
}

export async function setup() {
  return { framed: true };
}
