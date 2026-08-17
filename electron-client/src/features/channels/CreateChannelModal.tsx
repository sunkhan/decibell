import { useState } from "react";
import { invoke } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";

/// Small create dialog opened from the sidebar section-header "+"
/// buttons (MANAGE_CHANNELS only — the button is permission-gated).
/// The server slugs the name into the channel id; success confirms
/// itself via the channel_list_updated push, denials via the global
/// channel_action_responded toast.
export default function CreateChannelModal({
  serverId,
  channelType,
  onClose,
}: {
  serverId: string;
  channelType: "text" | "voice";
  onClose: () => void;
}) {
  useEscapeToClose(onClose, true);
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);

  const canCreate = name.trim().length > 0 && !creating;

  const handleCreate = async () => {
    if (!canCreate) return;
    setCreating(true);
    try {
      await invoke("create_channel", {
        serverId,
        name: name.trim(),
        channelType,
        voiceBitrateKbps: 0,
      });
      onClose();
    } catch (err) {
      setCreating(false);
      toast.error("Create failed", String(err));
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65"
      onClick={onClose}
    >
      <div
        className="w-full max-w-[400px] animate-[cardIn_0.25s_ease] rounded-xl border border-border bg-bg-dark p-6 shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-1 font-display text-[18px] font-semibold text-text-primary">
          Create {channelType} channel
        </h2>
        <p className="mb-4 text-[12px] leading-[1.55] text-text-muted">
          {channelType === "text"
            ? "A new place for messages. The channel id is derived from the name."
            : "A new place to talk. The channel id is derived from the name."}
        </p>
        <label className="mb-1 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
          Name
        </label>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") handleCreate();
          }}
          maxLength={64}
          placeholder={channelType === "text" ? "bug-reports" : "Game Night"}
          className="w-full rounded-md border border-border bg-bg-light px-3 py-2 text-[13px] text-text-primary outline-none transition-colors focus:border-accent"
        />
        <div className="mt-5 flex gap-2.5">
          <button
            onClick={onClose}
            className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
          >
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!canCreate}
            className="flex-1 rounded-md bg-accent py-2.5 text-[13px] font-semibold text-on-accent transition-all hover:bg-accent-hover active:scale-[0.98] disabled:opacity-50"
          >
            {creating ? "Creating..." : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
