// The GPUI kit's settings screens (kit-settings at 1280 wide, kit-settings-rail at 900): the
// renderer's SettingsWindow parts composed as SettingsModal composes them, an Audio-like tab with
// a draft and the SaveBar.
import Select from "@app/components/Select";
import Switch from "@app/components/Switch";
import {
  SETTINGS_BACKDROP,
  SETTINGS_BODY,
  SETTINGS_FRAME,
  SETTINGS_PANE,
  SettingsNav,
  SettingsNavFoot,
  SettingsNavItem,
  SettingsNavLabel,
  SettingsPaneHeader,
} from "@app/components/SettingsWindow";
import { Field } from "@app/features/servers/channelSettings/ui";
import { Icon, SaveBar } from "@app/features/servers/tabs/settingsUi";
import { Framed } from "./_frame";

const icons = import.meta.glob<string>("../../../assets/icons/settings-tab/*.svg", { query: "?raw", import: "default", eager: true });
const tabIcon = (id: string) => {
  const svg = icons[`../../../assets/icons/settings-tab/${id}.svg`] ?? "";
  return <span className="flex" dangerouslySetInnerHTML={{ __html: svg.replace(/<!--.*?-->\s*/, "").replace(/width="24" height="24"/, 'width="16" height="16"') }} />;
};

const TABS = ["account", "devices", "appearance", "privacy", "audio", "keybinds", "network", "codecs", "about"];
const LABEL = (id: string) => id[0].toUpperCase() + id.slice(1);
const devices = [
  { value: 1, label: "System default" },
  { value: 2, label: "Default device" },
  { value: 3, label: "Headset (USB)", hint: "Stereo · 48 kHz" },
  { value: 4, label: "HDMI output", disabled: true },
  { value: 5, label: "Speakers" },
];
const EXTRA = ["Show link previews", "Unfiltered GIF search", "Compact mode", "Show timestamps", "Reduce motion", "Developer mode"];

function Row({ title, description, children }: { title: string; description: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 py-3">
      <div className="flex min-w-0 flex-col">
        <div className="text-[14px] font-medium text-text-primary">{title}</div>
        <div className="text-[12px] text-text-muted">{description}</div>
      </div>
      {children}
    </div>
  );
}

export function KitSettings() {
  return (
    <Framed>
      <div className="flex h-full w-full items-start bg-bg-primary p-6">
        <button className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover">Open settings</button>
      </div>
      <div className={SETTINGS_BACKDROP} style={{ backgroundColor: "rgba(0,0,0,0.65)" }}>
        <div className={SETTINGS_FRAME} style={{ opacity: 1, transform: "scale(1)" }}>
          <SettingsNav>
            <SettingsNavLabel>Settings</SettingsNavLabel>
            {TABS.map((id) => (
              <SettingsNavItem key={id} icon={tabIcon(id)} label={LABEL(id)} active={id === "audio"} dot={id === "audio"} onClick={() => {}} />
            ))}
            <SettingsNavFoot>
              <SettingsNavItem danger label="Leave Server" icon={<Icon name="logout" />} onClick={() => {}} />
            </SettingsNavFoot>
          </SettingsNav>
          <div className={SETTINGS_PANE}>
            <SettingsPaneHeader title="Audio" context="Daily Gaming" onClose={() => {}} />
            <div className={SETTINGS_BODY}>
              <div className="flex flex-col gap-6">
                <Field label="Output device">
                  <Select value={2} onChange={() => {}} options={devices} />
                </Field>
                <Row title="Notification sounds" description="Play a sound when a message arrives.">
                  <Switch checked onToggle={() => {}} />
                </Row>
                <Field
                  label="Push to talk release delay"
                  aside={<span className="rounded-sm bg-accent-soft px-2 py-0.5 text-[12px] font-medium text-accent-bright">120 ms</span>}
                  hint="Keeps your mic open briefly after you let go, so the end of a word isn't cut off."
                >
                  <div className="py-[5px]">
                    {/* `block`: the kit compares the slider itself. In AudioTab the input is an
                        inline-block in a block box, so it sits in a 24 px line box (track 14 px
                        under the line's top); a port of that tab keeps the offset. */}
                    <input
                      type="range"
                      min={0}
                      max={1000}
                      step={10}
                      value={120}
                      onChange={() => {}}
                      className="block h-[4px] w-full cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[14px] [&::-webkit-slider-thumb]:w-[14px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
                    />
                  </div>
                </Field>
                {EXTRA.map((title, i) => (
                  <Row key={title} title={title} description="A setting that makes the pane tall enough to scroll.">
                    <Switch checked={i % 2 === 0} onToggle={() => {}} />
                  </Row>
                ))}
                <SaveBar show nudge={0} saving={false} onReset={() => {}} onSave={() => {}} />
              </div>
            </div>
          </div>
        </div>
      </div>
    </Framed>
  );
}
