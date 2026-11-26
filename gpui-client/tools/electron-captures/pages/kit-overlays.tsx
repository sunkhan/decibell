// The GPUI kit's `kit-overlays`, drawn as the whole window (overlays clamp to it): an open
// Select, settingsUi's Popover menu, the settings rail's hover label, the toast stack.
import { useRef } from "react";
import Select from "@app/components/Select";
import ToastStack from "@app/components/ToastStack";
import { useToastStore } from "@app/stores/toastStore";
import { MenuDivider, MenuItem, Popover } from "@app/features/servers/tabs/settingsUi";
import audio from "../../../assets/icons/settings-tab/audio.svg?raw";
import { click } from "../runtime/harness";
import { Framed, TITLEBAR } from "./_frame";

export const size = { width: 1280, height: 800 };

const devices = [
  { value: 1, label: "System default" },
  { value: 2, label: "Default device" },
  { value: 3, label: "Headset (USB)", hint: "Stereo · 48 kHz" },
  { value: 4, label: "HDMI output", disabled: true },
  { value: 5, label: "Speakers" },
];

const Note = ({ x, y, children }: { x: number; y: number; children: string }) => (
  <div className="absolute text-[12px] text-text-muted" style={{ left: x, top: y }}>
    {children}
  </div>
);

export default function KitOverlays() {
  const trigger = useRef<HTMLDivElement>(null);
  return (
    <Framed>
      <div className="relative h-full w-full bg-bg-primary p-6">
        <div className="absolute left-6 top-6 w-[260px]">
          <div className="text-[12px] text-text-muted">Select · open</div>
          <div className="mt-2" ref={trigger}>
            <Select value={2} onChange={() => {}} options={devices} />
          </div>
        </div>
        <Note x={920} y={24}>Menu (settings popover)</Note>
        <Popover anchor={{ x: 920, y: TITLEBAR + 52 }} onClose={() => {}} className="w-[220px] p-1">
          <MenuItem icon="pencil" label="Edit nickname" onClick={() => {}} />
          <MenuItem icon="key" label="Copy user ID" onClick={() => {}} />
          <MenuDivider />
          <MenuItem icon="userX" label="Kick" danger onClick={() => {}} />
          <MenuItem icon="ban" label="Ban" danger disabled onClick={() => {}} />
        </Popover>
        <Note x={340} y={330}>Rail label</Note>
        <div className="absolute" style={{ left: 340, top: 358 }}>
          <button type="button" className="group relative flex h-10 w-10 items-center justify-center rounded-md bg-surface-hover text-text-secondary">
            <span className="text-text-secondary" dangerouslySetInnerHTML={{ __html: audio.replace(/<!--.*?-->\s*/, "").replace(/width="24" height="24"/, 'width="16" height="16"') }} />
            <span className="pointer-events-none absolute left-full top-1/2 ml-4 block -translate-y-1/2 whitespace-nowrap rounded-md border border-border bg-bg-darkest px-2.5 py-1.5 text-[13px] font-medium text-text-primary shadow-float">
              Audio
            </span>
          </button>
        </div>
        <Note x={920} y={330}>Menu rows · label, checkable, unfolding, slider</Note>
        <ToastStack />
      </div>
    </Framed>
  );
}

export async function setup() {
  const hour = 3_600_000;
  const push = useToastStore.getState().push;
  push({ severity: "error", title: "Couldn't send the message", body: "The server refused it: you are sending messages too quickly.", duration: hour });
  push({ severity: "success", title: "Invite link copied", duration: hour });
  push({ severity: "info", title: "A new version is ready", action: { label: "Restart", run: () => {} }, duration: hour });
  const button = document.querySelector<HTMLButtonElement>("button[role=combobox]")!;
  const r = button.getBoundingClientRect();
  await click(Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2));
  return { framed: true };
}

