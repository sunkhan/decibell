// The GPUI kit's `kit-forms`: the renderer's Switch, SegmentedControl, Select, channel-settings
// Field, SearchField and the class strings of its checkboxes, sliders and inputs.
import { useState } from "react";
import Switch from "@app/components/Switch";
import SegmentedControl from "@app/components/SegmentedControl";
import Select from "@app/components/Select";
import { Field, FIELD } from "@app/features/servers/channelSettings/ui";
import { Icon, SearchField } from "@app/features/servers/tabs/settingsUi";
import { Page, Row, Section } from "./_frame";

const devices = [
  { value: 1, label: "System default" },
  { value: 2, label: "Default device" },
  { value: 3, label: "Headset (USB)", hint: "Stereo · 48 kHz" },
  { value: 4, label: "HDMI output", disabled: true },
  { value: 5, label: "Speakers" },
];

const SLIDER =
  "h-[4px] w-full cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[14px] [&::-webkit-slider-thumb]:w-[14px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]";
const at = (f: number) => `calc(${f} * (100% - 14px) + 7px)`;

/// features/voice/StreamAudioAppPicker.tsx's row.
function CheckRow({ name, checked, onChange }: { name: string; checked: boolean; onChange?: () => void }) {
  return (
    <label className="flex cursor-pointer items-center gap-2.5 rounded-sm px-2 py-1.5 hover:bg-surface-hover">
      <input type="checkbox" checked={checked} onChange={onChange} className="peer sr-only" />
      <span
        aria-hidden="true"
        className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border peer-focus-visible:shadow-ring ${
          checked ? "border-accent bg-accent text-on-accent" : "border-text-muted/50 bg-bg-lighter"
        }`}
      >
        {checked && <Icon name="check" size={11} strokeWidth={3.5} />}
      </span>
      <span className="min-w-0 flex-1 truncate text-[13px] text-text-primary">{name}</span>
    </label>
  );
}

function FilledSlider({ value, ticks = [], disabled }: { value: number; ticks?: number[]; disabled?: boolean }) {
  return (
    <div className={`relative ${disabled ? "opacity-60" : ""}`}>
      <input
        type="range"
        min={0}
        max={100}
        value={value}
        disabled={disabled}
        onChange={() => {}}
        className="block h-1 w-full cursor-pointer appearance-none rounded-full accent-accent disabled:cursor-not-allowed [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
        style={{ background: `linear-gradient(to right, var(--color-accent) ${at(value / 100)}, var(--color-bg-lighter) 0)` }}
      />
      {ticks.map((k) => (
        <span
          key={k}
          className="pointer-events-none absolute top-1/2 h-2 w-px -translate-x-1/2 -translate-y-1/2 bg-text-muted/40"
          style={{ left: at(k / 100) }}
        />
      ))}
    </div>
  );
}

export default function KitForms() {
  const [checks, setChecks] = useState([false, true]);
  const [quality, setQuality] = useState("medium");
  const opts = [
    { value: "low", label: "Low" },
    { value: "medium", label: "Medium" },
    { value: "high", label: "High" },
  ];
  return (
    <Page>
      <Section title="Switch · off, on, disabled">
        <Row className="gap-4">
          <Switch checked={false} onToggle={() => {}} />
          <Switch checked onToggle={() => {}} />
          <Switch checked={false} disabled onToggle={() => {}} />
          <Switch checked disabled onToggle={() => {}} />
        </Row>
      </Section>
      <Section title="Checkbox">
        <Row className="gap-2">
          <div className="w-[200px]">
            <CheckRow name="Discord" checked={checks[0]} onChange={() => setChecks([!checks[0], checks[1]])} />
          </div>
          <div className="w-[200px]">
            <CheckRow name="Firefox" checked={checks[1]} onChange={() => setChecks([checks[0], !checks[1]])} />
          </div>
          <div className="w-[200px] opacity-50">
            <CheckRow name="Disabled" checked />
          </div>
          <span className="flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border border-accent bg-accent text-on-accent">
            <Icon name="check" size={11} strokeWidth={3.5} />
          </span>
        </Row>
      </Section>
      <Section title="Segmented control">
        <Row className="gap-6">
          <div className="w-[360px]">
            <SegmentedControl options={opts} value={quality} onChange={setQuality} />
          </div>
          <div className="w-[360px]">
            <SegmentedControl options={opts} value="high" onChange={() => {}} disabled />
          </div>
        </Row>
      </Section>
      <Section title="Slider · plain, filled with ticks, disabled">
        <Row className="gap-8">
          <div className="w-[320px] py-[5px]">
            <input type="range" min={0} max={100} value={30} onChange={() => {}} className={`block ${SLIDER}`} />
          </div>
          <div className="w-[320px] py-[5px]">
            <FilledSlider value={60} ticks={[25, 50, 75]} />
          </div>
          <div className="w-[320px] py-[5px]">
            <FilledSlider value={40} disabled />
          </div>
        </Row>
      </Section>
      <Section title="Select · md, placeholder, sm, sunken, disabled">
        <Row className="items-start">
          <div className="w-[240px]">
            <Select value={2} onChange={() => {}} options={devices} icon={<Icon name="speaker" size={14} />} />
          </div>
          <div className="w-[200px]">
            <Select value={null} onChange={() => {}} options={devices} placeholder="Choose a device…" />
          </div>
          <div className="w-[140px]">
            <Select value={1} onChange={() => {}} size="sm" options={[{ value: 1, label: "Opus" }, { value: 2, label: "PCM" }]} />
          </div>
          <div className="w-[160px]">
            <Select value={1} onChange={() => {}} size="sm" variant="sunken" options={[{ value: 1, label: "Rust" }, { value: 2, label: "TypeScript" }]} />
          </div>
          <div className="w-[200px]">
            <Select value={1} onChange={() => {}} options={devices} disabled />
          </div>
        </Row>
      </Section>
      <Section title="Fields · value, hint, error, dialog label">
        <Row className="items-start gap-6">
          <div className="w-[260px]">
            <Field label="Server name">
              <input className={`h-10 px-3 ${FIELD}`} defaultValue="Daily Gaming" />
            </Field>
          </div>
          <div className="w-[260px]">
            <Field label="Topic" hint="Shown under the channel's name.">
              <input className={`h-10 px-3 ${FIELD}`} placeholder="What's this channel about?" />
            </Field>
          </div>
          <div className="w-[260px]">
            <Field label="Invite code">
              <input className={`h-10 px-3 ${FIELD} !border-error`} defaultValue="gaming" />
            </Field>
            <div className="mt-2 text-[12px] leading-[1.55] text-error">That code is taken.</div>
          </div>
          <div className="w-[260px]">
            <label className="mb-1.5 block text-[12px] font-medium text-text-secondary">Max uses</label>
            <SearchField value="" onChange={() => {}} placeholder="Search members" />
          </div>
        </Row>
      </Section>
    </Page>
  );
}

export async function setup() {
  const rect = (el: Element) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, height: r.height, left: r.left, width: r.width };
  };
  return {
    sliders: [...document.querySelectorAll('input[type="range"]')].map((el) => ({
      rect: rect(el),
      parent: rect(el.parentElement!),
      margin: getComputedStyle(el).margin,
      display: getComputedStyle(el).display,
    })),
    selects: [...document.querySelectorAll("button[role=combobox]")].map(rect),
  };
}
