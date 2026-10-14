import Select from "../../../components/Select";
import type { ChannelInfo } from "../../../types";
import type { IconName } from "../tabs/settingsUi";
import { Group, Notice, Row } from "./ui";
import type { ChannelDraft, RetentionField } from "./draft";

const PRESETS = [
  { label: "Forever", days: 0 },
  { label: "1 day", days: 1 },
  { label: "7 days", days: 7 },
  { label: "30 days", days: 30 },
  { label: "90 days", days: 90 },
  { label: "1 year", days: 365 },
];

const KINDS: Array<{ field: RetentionField; icon: IconName; label: string; hint: string }> = [
  { field: "retentionDaysText", icon: "message", label: "Messages", hint: "The message itself, with any attachments still on it" },
  { field: "retentionDaysImage", icon: "image", label: "Images", hint: "Photos and other still images" },
  { field: "retentionDaysVideo", icon: "film", label: "Videos", hint: "Recorded clips and screen captures" },
  { field: "retentionDaysDocument", icon: "file", label: "Files", hint: "PDFs, archives and other documents" },
  { field: "retentionDaysAudio", icon: "music", label: "Audio", hint: "Voice notes and other audio files" },
];

export default function RetentionTab({
  channel,
  draft,
  change,
  editable,
  onWipe,
}: {
  channel: ChannelInfo;
  draft: ChannelDraft;
  change: (patch: Partial<ChannelDraft>) => void;
  editable: boolean;
  onWipe: () => void;
}) {
  return (
    <div className="flex flex-col gap-7">
      {!editable && (
        <Notice icon="lock">You need Manage Channels in this channel to change these.</Notice>
      )}
      <p className="-mb-2 text-[13px] leading-[1.6] text-text-secondary">
        How long #{channel.name} keeps what's posted. An expired message goes with its attachments.
        An expired attachment leaves a placeholder, so readers can still see something was there.
      </p>

      <Group label="Keep for">
        {KINDS.map((k) => {
          const value = draft.retention[k.field];
          const preset = PRESETS.some((p) => p.days === value);
          return (
            <Row
              key={k.field}
              icon={k.icon}
              title={k.label}
              description={k.hint}
              control={
                <div className="w-36 shrink-0">
                  {/* A value set outside the presets (server config) shows
                      as its own entry; choosing it again is a no-op. */}
                  <Select
                    size="sm"
                    aria-label={`Keep ${k.label.toLowerCase()} for`}
                    disabled={!editable}
                    value={value}
                    searchable={false}
                    onChange={(v) => change({ retention: { ...draft.retention, [k.field]: v } })}
                    options={[
                      ...PRESETS.map((p) => ({ value: p.days, label: p.label })),
                      ...(preset ? [] : [{ value, label: `${value} days`, hint: "Custom" }]),
                    ]}
                  />
                </div>
              }
            />
          );
        })}
      </Group>

      {editable && (
        <Group label="Clean up">
          <Row
            icon="eraser"
            tone="danger"
            title="Wipe channel history"
            description={`Deletes every message and attachment in #${channel.name} now. The channel, its settings and its permissions stay.`}
            control={
              <button
                type="button"
                onClick={onWipe}
                className="shrink-0 rounded-sm border border-error/40 px-4 py-2 text-[13px] font-semibold text-error hover:border-error/70 hover:bg-error/10"
              >
                Wipe history…
              </button>
            }
          />
        </Group>
      )}
    </div>
  );
}
