import Select from "../../../components/Select";
import type { ChannelInfo } from "../../../types";
import { Icon } from "../tabs/settingsUi";
import { TOPIC_MAX_BYTES, normalizeTopicInput, topicBytes } from "../channelTopic";
import { clampLine } from "../../../lib/utf8";
import { CHANNEL_NAME_MAX_BYTES } from "../limits";
import { FIELD, Field, Group, Notice, Pill, Row, channelIcon } from "./ui";
import type { ChannelDraft } from "./draft";

const SLOWMODE = [
  { value: 0, label: "Off" },
  { value: 5, label: "5 seconds" },
  { value: 10, label: "10 seconds" },
  { value: 30, label: "30 seconds" },
  { value: 60, label: "1 minute" },
  { value: 300, label: "5 minutes" },
  { value: 900, label: "15 minutes" },
  { value: 3600, label: "1 hour" },
  { value: 21600, label: "6 hours" },
];

/// Reference ticks on the bitrate slider (kbps). Purely visual: the slider
/// itself is continuous (step 1).
const BITRATE_MARKS = [32, 64, 128, 192, 256];
const BITRATE_MIN = 16;
const BITRATE_MAX = 320;
const BITRATE_DEFAULT = 64;

/// Thumb-centre space: the 14px thumb's centre only travels
/// [7px, 100% - 7px], so ticks sit on that range, not the raw track.
const frac = (k: number) => (k - BITRATE_MIN) / (BITRATE_MAX - BITRATE_MIN);
const tickLeft = (k: number) => `calc(${frac(k)} * (100% - 14px) + 7px)`;

export default function OverviewTab({
  channel,
  draft,
  change,
  editable,
}: {
  channel: ChannelInfo;
  draft: ChannelDraft;
  change: (patch: Partial<ChannelDraft>) => void;
  editable: boolean;
}) {
  const isText = channel.type === "text";
  const isVoice = channel.type === "voice";
  const left = TOPIC_MAX_BYTES - topicBytes(draft.topic);
  return (
    <div className="flex max-w-2xl flex-col gap-7">
      {!editable && (
        <Notice icon="lock">You need Manage Channels in this channel to change these.</Notice>
      )}

      <Field label={channel.type === "category" ? "Category name" : "Channel name"}>
        <div className="relative">
          <Icon
            name={channelIcon(channel.type)}
            size={15}
            className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-text-muted"
          />
          <input
            value={draft.name}
            disabled={!editable}
            onChange={(e) => change({ name: clampLine(e.target.value, CHANNEL_NAME_MAX_BYTES) })}
            maxLength={64}
            spellCheck={false}
            className={`${FIELD} h-10 pl-9 pr-3`}
          />
        </div>
      </Field>

      {isText && (
        <Field
          label="Topic"
          aside={
            left < 100 && (
              <span className={`font-meta text-meta tabular-nums ${left < 20 ? "text-warning" : "text-text-muted"}`}>
                {left}
              </span>
            )
          }
          hint={
            channel.encrypted ? (
              <span className="inline-flex items-center gap-1.5">
                <Icon name="lockOpen" size={13} />
                Topics aren't encrypted. The server host can read them.
              </span>
            ) : (
              "Shown next to the channel name at the top of the chat."
            )
          }
        >
          <textarea
            value={draft.topic}
            disabled={!editable}
            onChange={(e) => change({ topic: normalizeTopicInput(e.target.value) })}
            onKeyDown={(e) => {
              // One line: Enter would only become a space.
              if (e.key === "Enter") e.preventDefault();
            }}
            rows={3}
            placeholder="Let everyone know what this channel is for"
            className={`${FIELD} block resize-none px-3 py-2.5 leading-5`}
          />
        </Field>
      )}

      {isText && (
        <Group label="Messages">
          <Row
            icon="hourglass"
            title="Slowmode"
            description="Members wait this long between messages. Anyone who can Manage Messages here is exempt."
            control={
              <div className="w-40 shrink-0">
                <Select
                  size="sm"
                  aria-label="Slowmode"
                  value={draft.slowmode}
                  disabled={!editable}
                  onChange={(v) => change({ slowmode: v })}
                  searchable={false}
                  options={SLOWMODE}
                />
              </div>
            }
          />
          <Row
            icon={channel.encrypted ? "lock" : "lockOpen"}
            tone={channel.encrypted ? "good" : "neutral"}
            title="End-to-end encryption"
            description={
              <>
                {channel.encrypted
                  ? "Messages and attachments are sealed on members' devices with keys the server never holds."
                  : "Messages are stored on the server as plain text."}{" "}
                Set when the channel was created; it can't be changed.
              </>
            }
            control={
              channel.encrypted ? (
                <Pill tone="good" icon="lock">On</Pill>
              ) : (
                <Pill tone="muted">Off</Pill>
              )
            }
          />
        </Group>
      )}

      {isVoice && (
        <Group label="Voice">
          <Row
            icon="waves"
            title="Bitrate"
            description="Higher sounds better and uses more bandwidth for everyone listening. Connected members retune right away."
            control={
              <div className="flex shrink-0 items-center gap-2">
                {draft.bitrate !== 0 && editable && (
                  <button
                    type="button"
                    onClick={() => change({ bitrate: 0 })}
                    className="rounded-sm px-2 py-1 text-[12px] font-semibold text-text-muted hover:bg-surface-hover hover:text-text-primary"
                  >
                    Reset
                  </button>
                )}
                <span className="inline-flex h-8 min-w-24 items-baseline justify-center gap-1 rounded-md border border-border-divider bg-bg-lighter px-2.5 pt-1.5 font-meta tabular-nums">
                  <span className="text-[14px] font-semibold text-text-primary">
                    {draft.bitrate === 0 ? BITRATE_DEFAULT : draft.bitrate}
                  </span>
                  <span className="text-[12px] text-text-muted">kbps</span>
                </span>
              </div>
            }
            below={
              <div className={`mt-4 pl-12.5 ${editable ? "" : "opacity-60"}`}>
                <div className="relative">
                  <input
                    type="range"
                    min={BITRATE_MIN}
                    max={BITRATE_MAX}
                    step={1}
                    aria-label="Bitrate"
                    disabled={!editable}
                    // 0 = "client default": park the thumb at the default's
                    // effective value; the first drag sets an explicit
                    // bitrate, Reset returns to 0.
                    value={draft.bitrate === 0 ? BITRATE_DEFAULT : draft.bitrate}
                    onChange={(e) => change({ bitrate: Number(e.target.value) })}
                    // The settings slider (AudioTab), with the run up to the
                    // thumb filled in the accent.
                    className="block h-1 w-full cursor-pointer appearance-none rounded-full accent-accent disabled:cursor-not-allowed [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
                    style={{
                      background: `linear-gradient(to right, var(--color-accent) ${tickLeft(draft.bitrate || BITRATE_DEFAULT)}, var(--color-bg-lighter) 0)`,
                    }}
                  />
                  {BITRATE_MARKS.map((k) => (
                    <span
                      key={k}
                      className="pointer-events-none absolute top-1/2 h-2 w-px -translate-x-1/2 -translate-y-1/2 bg-text-muted/40"
                      style={{ left: tickLeft(k) }}
                    />
                  ))}
                </div>
                <div className="relative mt-2 h-3.5 font-meta text-[11px] leading-none tabular-nums text-text-muted">
                  <span className="absolute left-0">{BITRATE_MIN}</span>
                  {BITRATE_MARKS.map((k) => (
                    <span
                      key={k}
                      className={`absolute -translate-x-1/2 ${
                        k === BITRATE_DEFAULT ? "font-semibold text-text-secondary" : ""
                      }`}
                      style={{ left: tickLeft(k) }}
                    >
                      {k}
                    </span>
                  ))}
                  <span className="absolute right-0">{BITRATE_MAX}</span>
                </div>
                {draft.bitrate === 0 && (
                  <div className="mt-3 text-[12px] text-text-muted">
                    Using the default, {BITRATE_DEFAULT} kbps. Drag to set this channel's own.
                  </div>
                )}
              </div>
            }
          />
          <Row
            icon="lock"
            tone="good"
            title="End-to-end encryption"
            description="Voice and screen shares are always sealed on members' devices. Stream preview thumbnails aren't."
            control={<Pill tone="good" icon="lock">Always on</Pill>}
          />
        </Group>
      )}
    </div>
  );
}
