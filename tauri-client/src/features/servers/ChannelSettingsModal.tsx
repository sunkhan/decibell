import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import type { Channel } from "../../types";

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
      className={`flex items-center gap-3 rounded-[10px] border px-3 py-2.5 transition-colors ${
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
          className="appearance-none rounded-md border border-border bg-bg-lighter px-3 py-1.5 pr-8 text-[12px] text-text-primary outline-none transition-all hover:border-white/[0.1] focus:border-accent disabled:cursor-not-allowed"
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
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const serverOwner = useChatStore((s) => s.serverOwner);
  const currentUser = useAuthStore((s) => s.username);

  const channel: Channel | undefined = useMemo(() => {
    if (!activeServerId || !activeChannelId) return undefined;
    return channelsByServer[activeServerId]?.find((c) => c.id === activeChannelId);
  }, [activeServerId, activeChannelId, channelsByServer]);

  const isOwner =
    !!activeServerId && !!currentUser && serverOwner[activeServerId] === currentUser;

  const [draft, setDraft] = useState<Record<RetentionField, number>>({
    retentionDaysText: 0,
    retentionDaysImage: 0,
    retentionDaysVideo: 0,
    retentionDaysDocument: 0,
    retentionDaysAudio: 0,
  });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset draft whenever the modal opens or the underlying channel changes.
  useEffect(() => {
    if (activeModal !== "channel-settings" || !channel) return;
    setDraft({
      retentionDaysText: presetValue(channel.retentionDaysText),
      retentionDaysImage: presetValue(channel.retentionDaysImage),
      retentionDaysVideo: presetValue(channel.retentionDaysVideo),
      retentionDaysDocument: presetValue(channel.retentionDaysDocument),
      retentionDaysAudio: presetValue(channel.retentionDaysAudio),
    });
    setError(null);
  }, [activeModal, channel?.id]);  // eslint-disable-line react-hooks/exhaustive-deps

  if (activeModal !== "channel-settings" || !channel || !activeServerId) return null;

  const setField = (field: RetentionField, value: number) => {
    setDraft((d) => ({ ...d, [field]: value }));
  };

  const dirty =
    !!channel &&
    (draft.retentionDaysText !== channel.retentionDaysText ||
      draft.retentionDaysImage !== channel.retentionDaysImage ||
      draft.retentionDaysVideo !== channel.retentionDaysVideo ||
      draft.retentionDaysDocument !== channel.retentionDaysDocument ||
      draft.retentionDaysAudio !== channel.retentionDaysAudio);

  const handleSave = async () => {
    if (!isOwner) return;
    setSaving(true);
    setError(null);
    try {
      await invoke("update_channel_retention", {
        serverId: activeServerId,
        channelId: channel.id,
        retentionDaysText: draft.retentionDaysText,
        retentionDaysImage: draft.retentionDaysImage,
        retentionDaysVideo: draft.retentionDaysVideo,
        retentionDaysDocument: draft.retentionDaysDocument,
        retentionDaysAudio: draft.retentionDaysAudio,
      });
      closeModal();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/65"
      onClick={closeModal}
    >
      <div
        className="flex w-full max-w-[480px] animate-[cardIn_0.25s_ease] flex-col overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)]"
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
            className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div className="flex-1 px-6 py-5">
          <div className="mb-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Retention
          </div>
          <p className="mb-4 text-[12px] leading-relaxed text-text-muted">
            Text retention governs the message row itself. Attachment retention
            removes the file but leaves a tombstone so readers can see what used
            to be there. Forever keeps content indefinitely.
          </p>

          <div className="flex flex-col gap-2">
            <RetentionRow
              label="Text messages"
              value={draft.retentionDaysText}
              onChange={(v) => setField("retentionDaysText", v)}
            />
            <RetentionRow
              label="Image attachments"
              hint="Applies once attachments ship"
              value={draft.retentionDaysImage}
              disabled
              onChange={(v) => setField("retentionDaysImage", v)}
            />
            <RetentionRow
              label="Video attachments"
              hint="Applies once attachments ship"
              value={draft.retentionDaysVideo}
              disabled
              onChange={(v) => setField("retentionDaysVideo", v)}
            />
            <RetentionRow
              label="Document attachments"
              hint="Applies once attachments ship"
              value={draft.retentionDaysDocument}
              disabled
              onChange={(v) => setField("retentionDaysDocument", v)}
            />
            <RetentionRow
              label="Audio attachments"
              hint="Applies once attachments ship"
              value={draft.retentionDaysAudio}
              disabled
              onChange={(v) => setField("retentionDaysAudio", v)}
            />
          </div>

          {error && (
            <p className="mt-3 text-[12px] text-error">{error}</p>
          )}
          {!isOwner && (
            <p className="mt-3 text-[12px] text-text-muted">
              Only the server owner can edit these.
            </p>
          )}
        </div>

        <div className="flex shrink-0 gap-2 border-t border-border-divider px-6 py-4">
          <button
            onClick={closeModal}
            className="flex-1 rounded-[10px] bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={saving || !dirty || !isOwner}
            className="flex-1 rounded-[10px] bg-accent py-2.5 text-[13px] font-semibold text-white transition-all hover:bg-accent-hover active:scale-[0.98] disabled:opacity-50"
          >
            {saving ? "Saving..." : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
