import { useEffect, useRef, useState } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import ConfirmModal from "../../components/ConfirmModal";
import type { ChannelUpdatedPayload } from "../../types";
import { TOPIC_MAX_BYTES, normalizeTopicInput, topicBytes } from "./channelTopic";

const NO_ANSWER_MS = 8000;

/// The welcome banner's "Edit topic": just the topic, without the rest of
/// channel settings. Saved through CHANNEL_UPDATE_REQ (MANAGE_CHANNELS);
/// the channel_updated echo confirms it, and failures reach only us.
export default function EditTopicModal() {
  const open = useUiStore((s) => s.activeModal === "edit-topic");
  const closeModal = useUiStore((s) => s.closeModal);
  const serverId = useChatStore((s) => s.activeServerId);
  const channelId = useUiStore((s) => s.channelSettingsChannelId);
  const channel = useChatStore((s) =>
    serverId && channelId
      ? s.channelsByServer[serverId]?.find((c) => c.id === channelId) ?? null
      : null,
  );
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The in-flight save's cleanup, so closing mid-save drops its answer.
  const pendingRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!open) return;
    setDraft(channel?.topic ?? "");
    setSaving(false);
    setError(null);
    return () => {
      pendingRef.current?.();
      pendingRef.current = null;
    };
  }, [open, channel?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const topic = draft.trim();
  const unchanged = topic === (channel?.topic ?? "");
  const remaining = TOPIC_MAX_BYTES - topicBytes(draft);

  const save = () => {
    if (!serverId || !channel || saving || unchanged) return;
    setSaving(true);
    setError(null);
    let done = false;
    let unlisten: (() => void) | null = null;
    const finish = (err: string | null) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      unlisten?.();
      pendingRef.current = null;
      setSaving(false);
      if (err) setError(err);
      else closeModal();
    };
    const timer = setTimeout(() => finish("The server didn't answer."), NO_ANSWER_MS);
    pendingRef.current = () => {
      done = true;
      clearTimeout(timer);
      unlisten?.();
    };
    listen<ChannelUpdatedPayload>("channel_updated", (e) => {
      const p = e.payload;
      if (p.serverId !== serverId) return;
      // Successes are broadcast to everyone; only this channel's counts.
      if (p.success && p.channel?.id !== channel.id) return;
      finish(p.success ? null : p.message || "Couldn't save the topic.");
    })
      .then((fn) => {
        if (done) {
          fn();
          return;
        }
        unlisten = fn;
        // CHANNEL_UPDATE_REQ always carries the retention snapshot; send
        // the channel's own values so only the topic changes.
        return invoke("update_channel_retention", {
          serverId,
          channelId: channel.id,
          retentionDaysText: channel.retentionDaysText,
          retentionDaysImage: channel.retentionDaysImage,
          retentionDaysVideo: channel.retentionDaysVideo,
          retentionDaysDocument: channel.retentionDaysDocument,
          retentionDaysAudio: channel.retentionDaysAudio,
          topic,
        });
      })
      .catch((err) => finish(String(err)));
  };

  return (
    <ConfirmModal
      open={open && !!channel}
      title="Edit topic"
      confirmLabel={saving ? "Saving…" : "Save"}
      confirmTone="accent"
      confirmDisabled={saving || unchanged}
      onConfirm={save}
      onCancel={closeModal}
    >
      <p className="mt-1 truncate text-[12px] text-text-muted">#{channel?.name}</p>
      <textarea
        value={draft}
        onChange={(e) => setDraft(normalizeTopicInput(e.target.value))}
        onFocus={(e) => {
          const end = e.currentTarget.value.length;
          e.currentTarget.setSelectionRange(end, end);
        }}
        autoFocus
        rows={3}
        placeholder="Let everyone know what this channel is for"
        className="mt-4 w-full resize-none rounded-md border border-border bg-bg-light px-3 py-2 text-[13px] leading-5 text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent"
      />
      <div className="mt-1.5 flex min-h-4 items-start justify-between gap-3 text-[11px] leading-4">
        {/* Topics are channel metadata, stored in the clear like the name
            (owner decision) — say so where the lock badge sits beside it. */}
        {error ? (
          <span className="text-error">{error}</span>
        ) : channel?.encrypted ? (
          <span className="text-text-muted">Topics aren't encrypted. The server host can read them.</span>
        ) : (
          <span />
        )}
        {remaining < 100 && <span className="shrink-0 text-text-muted">{remaining}</span>}
      </div>
    </ConfirmModal>
  );
}
