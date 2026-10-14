import { useState } from "react";
import { invoke } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import Switch from "../../components/Switch";

/// Create dialog for channels AND categories, opened from the
/// category-header "+" and the channel-list context menu
/// (MANAGE_CHANNELS only — every entry point is permission-gated).
/// The server slugs the name into the id; success confirms itself via
/// the channel_list_updated push, denials via the global
/// channel_action_responded toast.
export default function CreateChannelModal({
  serverId,
  channelType,
  categoryId,
  allowTypeChoice = false,
  onClose,
}: {
  serverId: string;
  channelType: "text" | "voice" | "category";
  /// Category to create the channel inside (end of its block).
  /// Undefined = end of the uncategorized area.
  categoryId?: string;
  /// Show a Text/Voice toggle (used by the category-header "+", where
  /// the kind isn't implied by the entry point).
  allowTypeChoice?: boolean;
  onClose: () => void;
}) {
  useEscapeToClose(onClose, true);
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);
  const [type, setType] = useState<"text" | "voice" | "category">(channelType);
  // Text channels only, and only here: encryption is fixed for a channel's
  // life so its history is never half sealed. Off by default (owner
  // decision — server-side search keeps working unless someone opts in).
  const [encrypted, setEncrypted] = useState(false);

  const isCategory = type === "category";
  const canCreate = name.trim().length > 0 && !creating;

  const handleCreate = async () => {
    if (!canCreate) return;
    setCreating(true);
    try {
      await invoke("create_channel", {
        serverId,
        name: name.trim(),
        channelType: type,
        voiceBitrateKbps: 0,
        categoryId: isCategory ? undefined : categoryId,
        encrypted: type === "text" && encrypted,
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
          {isCategory ? "Create category" : `Create ${type} channel`}
        </h2>
        <p className="mb-4 text-[12px] leading-[1.55] text-text-muted">
          {isCategory
            ? "A collapsible group for channels. Drag channels into it to organize the sidebar."
            : type === "text"
              ? "A new place for messages. The channel id is derived from the name."
              : "A new place to talk. The channel id is derived from the name."}
        </p>
        {allowTypeChoice && !isCategory && (
          <div className="mb-4 flex gap-1 rounded-md bg-bg-light p-1">
            {(["text", "voice"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setType(t)}
                className={`flex-1 cursor-pointer rounded-sm py-1.5 text-[12px] font-medium capitalize transition-colors ${
                  type === t
                    ? "bg-accent-soft text-accent-bright"
                    : "text-text-muted hover:text-text-secondary"
                }`}
              >
                {t}
              </button>
            ))}
          </div>
        )}
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
          placeholder={
            isCategory ? "Gaming" : type === "text" ? "bug-reports" : "Game Night"
          }
          className="w-full rounded-md border border-border bg-bg-light px-3 py-2 text-[13px] text-text-primary outline-none transition-colors focus:border-accent"
        />
        {type === "text" && (
          <label className="mt-4 flex cursor-pointer items-start justify-between gap-4 rounded-md border border-border-divider bg-bg-light px-3 py-2.5">
            <div className="min-w-0">
              <div className="text-[13px] font-medium text-text-primary">End-to-end encrypted</div>
              <div className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">
                Messages and files are sealed on members' devices, so the server can't read them.
                Members need encryption set up, and search won't cover it. You can't change this
                later.
              </div>
            </div>
            <Switch checked={encrypted} onToggle={() => setEncrypted((v) => !v)} />
          </label>
        )}
        <div className="mt-5 flex gap-2.5">
          <button
            onClick={onClose}
            className="flex-1 cursor-pointer rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
          >
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!canCreate}
            className="flex-1 cursor-pointer rounded-md bg-accent py-2.5 text-[13px] font-semibold text-on-accent transition-all hover:bg-accent-hover active:scale-[0.98] disabled:opacity-50"
          >
            {creating ? "Creating..." : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
