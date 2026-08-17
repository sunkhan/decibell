import { useEffect, useMemo, useState } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import { PERM, usePermission } from "./permissions";
import type { ChannelInfo } from "../../types";

type RetentionField =
  | "retentionDaysText"
  | "retentionDaysImage"
  | "retentionDaysVideo"
  | "retentionDaysDocument"
  | "retentionDaysAudio";

interface Preset {
  label: string;
  days: number; // 0 = forever
}

const PRESETS: Preset[] = [
  { label: "Forever", days: 0 },
  { label: "1 day", days: 1 },
  { label: "7 days", days: 7 },
  { label: "30 days", days: 30 },
  { label: "90 days", days: 90 },
  { label: "1 year", days: 365 },
];

function presetValue(days: number): number {
  // Collapse any positive value to the closest defined preset; 0 stays 0.
  if (days <= 0) return 0;
  const exact = PRESETS.find((p) => p.days === days);
  if (exact) return exact.days;
  return days;
}

function RetentionRow({
  label,
  hint,
  value,
  disabled,
  onChange,
}: {
  label: string;
  hint?: string;
  value: number;
  disabled?: boolean;
  onChange: (v: number) => void;
}) {
  const presetMatch = PRESETS.some((p) => p.days === value);
  return (
    <div
      className={`flex items-center gap-3 rounded-md border px-3 py-2.5 transition-colors ${
        disabled ? "border-border-divider bg-bg-light/30 opacity-60" : "border-border-divider bg-bg-light"
      }`}
    >
      <div className="min-w-0 flex-1">
        <div className="text-[13px] font-medium text-text-primary">{label}</div>
        {hint && (
          <div className="mt-0.5 truncate text-[11px] text-text-muted">{hint}</div>
        )}
      </div>
      <div className="relative shrink-0">
        <select
          value={presetMatch ? String(value) : "custom"}
          disabled={disabled}
          onChange={(e) => {
            const v = e.target.value;
            if (v === "custom") return;
            onChange(parseInt(v, 10));
          }}
          className="appearance-none rounded-sm border border-border bg-bg-lighter px-3 py-1.5 pr-8 text-[12px] text-text-primary outline-none transition-all hover:border-text-faint focus:border-accent disabled:cursor-not-allowed"
        >
          {PRESETS.map((p) => (
            <option key={p.days} value={p.days}>
              {p.label}
            </option>
          ))}
          {!presetMatch && <option value="custom">{value} days</option>}
        </select>
        <div className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-text-muted">
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </div>
      </div>
    </div>
  );
}

export default function ChannelSettingsModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  useEscapeToClose(closeModal, activeModal === "channel-settings");
  const activeServerId = useChatStore((s) => s.activeServerId);
  // Explicit target set by the per-row gear icon — the channel no
  // longer needs to be the active one.
  const targetChannelId = useUiStore((s) => s.channelSettingsChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);

  const channel: ChannelInfo | undefined = useMemo(() => {
    if (!activeServerId || !targetChannelId) return undefined;
    return channelsByServer[activeServerId]?.find((c) => c.id === targetChannelId);
  }, [activeServerId, targetChannelId, channelsByServer]);

  // Mirrors the server-side gates: retention/rename/delete/wipe are all
  // MANAGE_CHANNELS (owner implicitly included).
  const canManage = usePermission(activeServerId, PERM.MANAGE_CHANNELS);
  // The landing channel after auth must always exist — the server
  // refuses to delete the last text channel, so hide the button too.
  const isLastTextChannel =
    !!channel &&
    channel.type === "text" &&
    (activeServerId
      ? (channelsByServer[activeServerId] ?? []).filter((c) => c.type === "text")
          .length <= 1
      : false);

  const [draft, setDraft] = useState<Record<RetentionField, number>>({
    retentionDaysText: 0,
    retentionDaysImage: 0,
    retentionDaysVideo: 0,
    retentionDaysDocument: 0,
    retentionDaysAudio: 0,
  });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nameDraft, setNameDraft] = useState("");
  const [wipeConfirmOpen, setWipeConfirmOpen] = useState(false);
  const [wipeConfirmText, setWipeConfirmText] = useState("");
  const [wiping, setWiping] = useState(false);
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const [deleteConfirmText, setDeleteConfirmText] = useState("");
  const [deleting, setDeleting] = useState(false);

  // Reset draft whenever the modal opens or the underlying channel
  // changes. This component stays mounted across open/close (it
  // returns null when closed), so EVERY piece of transient state must
  // be re-initialized here — the in-flight flags included: a
  // successful delete closes the modal with `deleting` still true,
  // which left the next channel's delete button stuck on "Deleting…"
  // until an app restart.
  useEffect(() => {
    if (activeModal !== "channel-settings" || !channel) return;
    setDraft({
      retentionDaysText: presetValue(channel.retentionDaysText),
      retentionDaysImage: presetValue(channel.retentionDaysImage),
      retentionDaysVideo: presetValue(channel.retentionDaysVideo),
      retentionDaysDocument: presetValue(channel.retentionDaysDocument),
      retentionDaysAudio: presetValue(channel.retentionDaysAudio),
    });
    setNameDraft(channel.name);
    setError(null);
    setSaving(false);
    setWipeConfirmOpen(false);
    setWipeConfirmText("");
    setWiping(false);
    setDeleteConfirmOpen(false);
    setDeleteConfirmText("");
    setDeleting(false);
  }, [activeModal, channel?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // Surface the server's CHANNEL_WIPE_RES as a toast. The CHANNEL_WIPED
  // broadcast that follows clears the local message store via
  // useChatEvents, so we don't have to do anything else here.
  useEffect(() => {
    if (activeModal !== "channel-settings") return;
    const unlisten = listen<{
      serverId: string;
      channelId: string;
      success: boolean;
      message: string;
      deletedMessageCount: number;
      deletedAttachmentCount: number;
    }>("channel_wipe_responded", (event) => {
      if (event.payload.serverId !== activeServerId) return;
      if (event.payload.channelId !== channel?.id) return;
      setWiping(false);
      if (event.payload.success) {
        toast.success(
          "Channel history wiped",
          `Removed ${event.payload.deletedMessageCount.toLocaleString()} message(s) and ${event.payload.deletedAttachmentCount.toLocaleString()} attachment(s).`,
        );
        setWipeConfirmOpen(false);
        setWipeConfirmText("");
      } else {
        toast.error("Wipe failed", event.payload.message || "Unknown error.");
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [activeModal, activeServerId, channel?.id]);

  if (activeModal !== "channel-settings" || !channel || !activeServerId) return null;

  const setField = (field: RetentionField, value: number) => {
    setDraft((d) => ({ ...d, [field]: value }));
  };

  const retentionDirty =
    !!channel &&
    (draft.retentionDaysText !== channel.retentionDaysText ||
      draft.retentionDaysImage !== channel.retentionDaysImage ||
      draft.retentionDaysVideo !== channel.retentionDaysVideo ||
      draft.retentionDaysDocument !== channel.retentionDaysDocument ||
      draft.retentionDaysAudio !== channel.retentionDaysAudio);
  const nameDirty =
    !!channel && nameDraft.trim().length > 0 && nameDraft.trim() !== channel.name;
  const dirty = retentionDirty || nameDirty;

  const handleSave = async () => {
    if (!canManage) return;
    setSaving(true);
    setError(null);
    try {
      if (nameDirty) {
        await invoke("rename_channel", {
          serverId: activeServerId,
          channelId: channel.id,
          name: nameDraft.trim(),
        });
      }
      if (retentionDirty) {
        await invoke("update_channel_retention", {
          serverId: activeServerId,
          channelId: channel.id,
          retentionDaysText: draft.retentionDaysText,
          retentionDaysImage: draft.retentionDaysImage,
          retentionDaysVideo: draft.retentionDaysVideo,
          retentionDaysDocument: draft.retentionDaysDocument,
          retentionDaysAudio: draft.retentionDaysAudio,
        });
      }
      closeModal();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!canManage || deleting) return;
    if (deleteConfirmText !== channel.name) return;
    setDeleting(true);
    try {
      await invoke("delete_channel", {
        serverId: activeServerId,
        channelId: channel.id,
      });
      // Success confirmation is the CHANNEL_LIST_UPDATE broadcast (the
      // sidebar re-renders and the active channel switches); denials
      // surface via the global channel_action_responded toast. Reset
      // the flag before closing — this component survives the close.
      setDeleting(false);
      closeModal();
    } catch (err) {
      setDeleting(false);
      toast.error("Delete failed", String(err));
    }
  };

  const handleWipe = async () => {
    if (!canManage || wiping) return;
    if (wipeConfirmText !== channel.name) return;
    setWiping(true);
    try {
      await invoke("wipe_channel_history", {
        serverId: activeServerId,
        channelId: channel.id,
      });
      // No-op until the channel_wipe_responded event lands — see the
      // listener above, which handles success/error toasts and resets
      // the wiping flag.
    } catch (err) {
      setWiping(false);
      toast.error("Wipe failed", String(err));
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[85vh] w-full max-w-[480px] animate-[cardIn_0.25s_ease] flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center justify-between border-b border-border-divider px-6 py-5">
          <div className="min-w-0">
            <h2 className="font-display text-[18px] font-semibold text-text-primary">
              Channel settings
            </h2>
            <p className="truncate text-[12px] text-text-muted">
              #{channel.name}
            </p>
          </div>
          <button
            onClick={closeModal}
            className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-5 scrollbar-thin">
          {canManage && (
            <div className="mb-5">
              <div className="mb-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                Channel name
              </div>
              <input
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                maxLength={64}
                className="w-full rounded-md border border-border bg-bg-light px-3 py-2 text-[13px] text-text-primary outline-none transition-colors focus:border-accent"
              />
            </div>
          )}

          <div className="mb-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
            Retention
          </div>
          <p className="mb-4 text-[12px] leading-[1.55] text-text-muted">
            Text retention governs the message row itself. Attachment retention
            removes the file but leaves a tombstone so readers can see what used
            to be there. Forever keeps content indefinitely.
          </p>

          <div className="flex flex-col gap-2">
            <RetentionRow
              label="Text messages"
              hint="Removes the message and any attachments still on it"
              value={draft.retentionDaysText}
              onChange={(v) => setField("retentionDaysText", v)}
            />
            <RetentionRow
              label="Image attachments"
              hint="Photos and other still images"
              value={draft.retentionDaysImage}
              onChange={(v) => setField("retentionDaysImage", v)}
            />
            <RetentionRow
              label="Video attachments"
              hint="Recorded clips and screen captures"
              value={draft.retentionDaysVideo}
              onChange={(v) => setField("retentionDaysVideo", v)}
            />
            <RetentionRow
              label="Document attachments"
              hint="PDFs, archives, and other files"
              value={draft.retentionDaysDocument}
              onChange={(v) => setField("retentionDaysDocument", v)}
            />
            <RetentionRow
              label="Audio attachments"
              hint="Voice notes and other audio files"
              value={draft.retentionDaysAudio}
              onChange={(v) => setField("retentionDaysAudio", v)}
            />
          </div>

          {error && (
            <p className="mt-3 text-[12px] text-error">{error}</p>
          )}
          {!canManage && (
            <p className="mt-3 text-[12px] text-text-muted">
              You need the Manage Channels permission to edit these.
            </p>
          )}

          {canManage && (
            <div className="mt-6">
              <div className="mb-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-error">
                Danger zone
              </div>
              <div className="rounded-md border border-error/25 bg-error/5 p-3">
                <div className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="text-[13px] font-medium text-text-primary">
                      Wipe channel history
                    </div>
                    <p className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">
                      Permanently deletes every message and attachment in
                      #{channel.name}. The channel itself, members, and
                      retention settings stay. Cannot be undone.
                    </p>
                  </div>
                  {!wipeConfirmOpen && (
                    <button
                      onClick={() => setWipeConfirmOpen(true)}
                      className="shrink-0 rounded-sm border border-error/40 bg-error/10 px-3 py-1.5 text-[12px] font-semibold text-error transition-colors hover:border-error/70 hover:bg-error/20"
                    >
                      Wipe…
                    </button>
                  )}
                </div>

                {wipeConfirmOpen && (
                  <div className="mt-3 border-t border-error/20 pt-3">
                    <label className="mb-1.5 block text-[11px] text-text-muted">
                      Type <span className="font-mono text-text-primary">{channel.name}</span> to confirm:
                    </label>
                    <input
                      autoFocus
                      type="text"
                      value={wipeConfirmText}
                      onChange={(e) => setWipeConfirmText(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && wipeConfirmText === channel.name && !wiping) {
                          handleWipe();
                        }
                      }}
                      placeholder={channel.name}
                      className="w-full rounded-sm border border-border bg-bg-lighter px-2.5 py-1.5 text-[12px] text-text-primary outline-none transition-colors focus:border-error"
                    />
                    <div className="mt-2 flex gap-2">
                      <button
                        onClick={() => {
                          setWipeConfirmOpen(false);
                          setWipeConfirmText("");
                        }}
                        disabled={wiping}
                        className="flex-1 rounded-sm bg-bg-light py-1.5 text-[12px] font-medium text-text-primary transition-colors hover:bg-bg-lighter disabled:opacity-50"
                      >
                        Cancel
                      </button>
                      <button
                        onClick={handleWipe}
                        disabled={wiping || wipeConfirmText !== channel.name}
                        className="flex-1 rounded-sm bg-error py-1.5 text-[12px] font-semibold text-white transition-all hover:bg-error/85 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-40"
                      >
                        {wiping ? "Wiping…" : "Wipe history"}
                      </button>
                    </div>
                  </div>
                )}
              </div>

              {!isLastTextChannel && (
                <div className="mt-3 rounded-md border border-error/25 bg-error/5 p-3">
                  <div className="flex items-start gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="text-[13px] font-medium text-text-primary">
                        Delete channel
                      </div>
                      <p className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">
                        Removes #{channel.name} along with every message and
                        attachment in it. Cannot be undone.
                        {channel.type === "voice" &&
                          " The channel must be empty first."}
                      </p>
                    </div>
                    {!deleteConfirmOpen && (
                      <button
                        onClick={() => setDeleteConfirmOpen(true)}
                        className="shrink-0 rounded-sm border border-error/40 bg-error/10 px-3 py-1.5 text-[12px] font-semibold text-error transition-colors hover:border-error/70 hover:bg-error/20"
                      >
                        Delete…
                      </button>
                    )}
                  </div>

                  {deleteConfirmOpen && (
                    <div className="mt-3 border-t border-error/20 pt-3">
                      <label className="mb-1.5 block text-[11px] text-text-muted">
                        Type <span className="font-mono text-text-primary">{channel.name}</span> to confirm:
                      </label>
                      <input
                        autoFocus
                        type="text"
                        value={deleteConfirmText}
                        onChange={(e) => setDeleteConfirmText(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" && deleteConfirmText === channel.name && !deleting) {
                            handleDelete();
                          }
                        }}
                        placeholder={channel.name}
                        className="w-full rounded-sm border border-border bg-bg-lighter px-2.5 py-1.5 text-[12px] text-text-primary outline-none transition-colors focus:border-error"
                      />
                      <div className="mt-2 flex gap-2">
                        <button
                          onClick={() => {
                            setDeleteConfirmOpen(false);
                            setDeleteConfirmText("");
                          }}
                          disabled={deleting}
                          className="flex-1 rounded-sm bg-bg-light py-1.5 text-[12px] font-medium text-text-primary transition-colors hover:bg-bg-lighter disabled:opacity-50"
                        >
                          Cancel
                        </button>
                        <button
                          onClick={handleDelete}
                          disabled={deleting || deleteConfirmText !== channel.name}
                          className="flex-1 rounded-sm bg-error py-1.5 text-[12px] font-semibold text-white transition-all hover:bg-error/85 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-40"
                        >
                          {deleting ? "Deleting…" : "Delete channel"}
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        <div className="flex shrink-0 gap-2 border-t border-border-divider px-6 py-4">
          <button
            onClick={closeModal}
            className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={saving || !dirty || !canManage}
            className="flex-1 rounded-md bg-accent py-2.5 text-[13px] font-semibold text-on-accent transition-all hover:bg-accent-hover active:scale-[0.98] disabled:opacity-50"
          >
            {saving ? "Saving..." : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
