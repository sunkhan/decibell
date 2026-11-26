// Chromium's scrollbar under the app's CSS (globals.css: `* { scrollbar-width: thin;
// scrollbar-color: var(--t-scrollbar) transparent }` plus ::-webkit-scrollbar rules), and the real
// Switch. Measurement page for the GPUI scrollbar; setup() returns the boxes' metrics (saved as
// kit-scroll-<theme>.json).
//
//   A  40 rows, scrollTop 200            B  5 rows, [scrollbar-gutter:stable], no overflow
//   C  40 rows, at the bottom, pointer   D  the settings pane classes (SETTINGS_PANE), scrollTop 0
//      on the thumb (real mouseMove)     E  control: scrollbar-width/-color auto, scrollTop 200
//                                        F  24 000 px of content: the minimum thumb length
//
// Variants for measuring (run.sh --query k=v, into another --out): hover=thumb (default) | track |
// content | none | ethumb (E's thumb), press=1 (hold the left button down there).
import { useState } from "react";
import Switch from "@app/components/Switch";
import { hover, input } from "../runtime/harness";

const rows = (n: number) =>
  Array.from({ length: n }, (_, i) => (
    <div key={i} className="flex h-8 shrink-0 items-center px-3 text-[13px] text-text-secondary odd:bg-bg-secondary">
      Row {i + 1}
    </div>
  ));

const BOX = "absolute h-[400px] w-[300px] bg-bg-primary outline outline-1 outline-border";
const Label = ({ x, y, children }: { x: number; y: number; children: string }) => (
  <div className="absolute text-[12px] text-text-muted" style={{ left: x, top: y }}>
    {children}
  </div>
);

export default function KitScroll() {
  const [on, setOn] = useState(true);
  const [off, setOff] = useState(false);
  return (
    <div className="relative h-full w-full bg-bg-primary text-text-primary">
      <div className="absolute left-4 top-3 text-[14px] font-semibold">kit-scroll · Chromium scrollbars under globals.css · Switch</div>

      <Label x={16} y={40}>A · overflow-y-auto · scrollTop 200</Label>
      <div id="box-a" className={`${BOX} overflow-y-auto`} style={{ left: 16, top: 64 }}>{rows(40)}</div>

      <Label x={336} y={40}>B · gutter stable · no overflow</Label>
      <div id="box-b" className={`${BOX} overflow-y-auto [scrollbar-gutter:stable]`} style={{ left: 336, top: 64 }}>{rows(5)}</div>

      <Label x={656} y={40}>C · at the bottom · thumb hovered</Label>
      <div id="box-c" className={`${BOX} overflow-y-auto`} style={{ left: 656, top: 64 }}>{rows(40)}</div>

      <Label x={976} y={40}>D · SETTINGS_PANE · scrollTop 0</Label>
      <div className={`${BOX} flex flex-col`} style={{ left: 976, top: 64 }}>
        <div id="box-d" className="flex flex-1 flex-col overflow-y-auto scrollbar-thin [scrollbar-gutter:stable]">{rows(40)}</div>
      </div>

      <Label x={16} y={488}>Switch (off, on)</Label>
      <div className="absolute flex items-center gap-4" style={{ left: 16, top: 512 }}>
        <Switch checked={off} onToggle={() => setOff(!off)} label="off" />
        <Switch checked={on} onToggle={() => setOn(!on)} label="on" />
      </div>

      <Label x={336} y={488}>E · control: scrollbar-width/color auto</Label>
      <div id="box-e" className={`${BOX} overflow-y-auto [scrollbar-width:auto] [scrollbar-color:auto]`} style={{ left: 336, top: 512, height: 240 }}>
        {rows(40)}
      </div>

      <Label x={656} y={488}>F · 24 000 px of content (minimum thumb)</Label>
      <div id="box-f" className={`${BOX} overflow-y-auto`} style={{ left: 656, top: 512, height: 240 }}>
        <div className="h-[24000px] bg-[linear-gradient(var(--color-bg-secondary)_50%,transparent_50%)] bg-[length:100%_64px]" />
      </div>
    </div>
  );
}

function metrics(el: HTMLElement) {
  const r = el.getBoundingClientRect();
  const cs = getComputedStyle(el);
  return {
    rect: { left: r.left, top: r.top, width: r.width, height: r.height },
    offsetWidth: el.offsetWidth,
    clientWidth: el.clientWidth,
    scrollbarSpace: el.offsetWidth - el.clientWidth,
    offsetHeight: el.offsetHeight,
    clientHeight: el.clientHeight,
    scrollHeight: el.scrollHeight,
    scrollTop: el.scrollTop,
    scrollbarWidth: cs.scrollbarWidth,
    scrollbarColor: cs.scrollbarColor,
    scrollbarGutter: cs.scrollbarGutter,
  };
}

export async function setup() {
  const box = (id: string) => document.getElementById(id)!;
  box("box-a").scrollTop = 200;
  box("box-e").scrollTop = 200;
  const c = box("box-c");
  c.scrollTop = c.scrollHeight;

  const r = c.getBoundingClientRect();
  const bar = c.offsetWidth - c.clientWidth;
  const x = Math.round(r.right - Math.max(bar, 6) / 2);
  const e = box("box-e").getBoundingClientRect();
  const q = new URLSearchParams(location.search);
  const where = q.get("hover") ?? "thumb";
  const at: Record<string, [number, number]> = {
    thumb: [x, Math.round(r.bottom - 40)],
    track: [x, Math.round(r.top + 60)],
    content: [Math.round(r.left + 100), Math.round(r.top + 200)],
    none: [1270, 760],
    // E's 6 px ::-webkit-scrollbar thumb (scrollTop 200 of 1280, 240 tall): proves hover reaches
    // scrollbars offscreen, through its ::-webkit-scrollbar-thumb:hover colour.
    ethumb: [Math.round(e.right - 3), Math.round(e.top + 60)],
  };
  const [hx, hy] = at[where] ?? at.thumb;
  let seen: [number, number] | null = null;
  const onMove = (ev: MouseEvent) => (seen = [ev.clientX, ev.clientY]);
  window.addEventListener("mousemove", onMove, true);
  await hover(hx, hy);
  // ?press=1: hold the left button down on that spot (the thumb's pressed state).
  if (q.get("press") === "1") await input([{ type: "mouseDown", x: hx, y: hy, button: "left", clickCount: 1 }]);
  window.removeEventListener("mousemove", onMove, true);
  const hovered = [...document.querySelectorAll(":hover")].map((el) => el.id || el.tagName.toLowerCase());

  const root = getComputedStyle(document.documentElement);
  const v = (n: string) => root.getPropertyValue(n).trim();
  return {
    hover: { where, x: hx, y: hy, mousemoveSeenAt: seen, hovered },
    vars: { scrollbar: v("--t-scrollbar"), scrollbarHover: v("--t-scrollbar-hover"), chrome: v("--t-chrome"), sidebar: v("--t-sidebar") },
    boxes: Object.fromEntries(["box-a", "box-b", "box-c", "box-d", "box-e", "box-f"].map((id) => [id, metrics(box(id))])),
  };
}
