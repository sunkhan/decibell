// Helpers for a page's `setup()`. Import them from a page as "../runtime/harness".
//
// Real input goes through Electron's `webContents.sendInputEvent` (the runner's preload exposes
// `window.__harness.input`): use it when Chromium itself must see the pointer (scrollbar hover,
// :hover, native drag). DOM-level `el.click()` / `dispatchEvent` is enough for React handlers.
export { respond } from "./stub";

/** One event for Electron's sendInputEvent, or a pause: { type: "wait", ms }. */
export type InputEvent =
  | { type: "wait"; ms: number }
  | {
      type: "mouseMove" | "mouseDown" | "mouseUp" | "mouseEnter" | "mouseLeave";
      x: number;
      y: number;
      button?: "left" | "middle" | "right";
      clickCount?: number;
      modifiers?: string[];
    }
  | {
      type: "mouseWheel";
      x: number;
      y: number;
      // Sign is the opposite of the DOM's: deltaY -120 scrolls DOWN by one notch.
      deltaX?: number;
      deltaY?: number;
      canScroll?: boolean;
    }
  | { type: "keyDown" | "keyUp" | "char"; keyCode: string; modifiers?: string[] };

declare global {
  interface Window {
    __harness?: { input: (events: InputEvent[]) => Promise<void> };
  }
}

/** Waits `n` animation frames (two are enough for a style change to be painted). */
export function frames(n = 2): Promise<void> {
  return new Promise((resolve) => {
    const step = (left: number) => (left <= 0 ? resolve() : requestAnimationFrame(() => step(left - 1)));
    step(n);
  });
}

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Sends real input events through the runner, then waits two frames. */
export async function input(events: InputEvent[]): Promise<void> {
  if (!window.__harness) throw new Error("no runner: real input needs shoot.cjs's preload");
  await window.__harness.input(events);
  await frames(2);
}

/** Moves the real pointer to (x, y), in page CSS px. */
export const hover = (x: number, y: number) => input([{ type: "mouseMove", x, y }]);

/** A real left click at (x, y). */
export const click = (x: number, y: number) =>
  input([
    { type: "mouseMove", x, y },
    { type: "mouseDown", x, y, button: "left", clickCount: 1 },
    { type: "mouseUp", x, y, button: "left", clickCount: 1 },
  ]);

/** A real drag from a to b (mouseDown, moves carrying leftButtonDown, mouseUp). */
export function drag(a: [number, number], b: [number, number], steps = 8): Promise<void> {
  const ev: InputEvent[] = [
    { type: "mouseMove", x: a[0], y: a[1] },
    { type: "mouseDown", x: a[0], y: a[1], button: "left", clickCount: 1 },
  ];
  for (let i = 1; i <= steps; i++) {
    const x = Math.round(a[0] + ((b[0] - a[0]) * i) / steps);
    const y = Math.round(a[1] + ((b[1] - a[1]) * i) / steps);
    ev.push({ type: "mouseMove", x, y, modifiers: ["leftButtonDown"] });
  }
  ev.push({ type: "mouseUp", x: b[0], y: b[1], button: "left", clickCount: 1 });
  return input(ev);
}

/** Offscreen windows fire no focus events on el.focus(); this focuses and tells listeners. */
export function focusIn(el: HTMLElement): void {
  el.focus();
  el.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
  el.dispatchEvent(new FocusEvent("focus"));
}

/** The element matching `selector`, or a thrown error naming it. */
export function $(selector: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(selector);
  if (!el) throw new Error(`no element matches ${selector}`);
  return el;
}

/** Centre of an element in page px. */
export function centre(el: Element): [number, number] {
  const r = el.getBoundingClientRect();
  return [Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2)];
}
