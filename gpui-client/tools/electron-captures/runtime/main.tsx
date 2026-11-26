// Mounts one page: index.html?page=<name>&theme=<id>[&...].
// The stub must be the first import: app modules may touch window.decibell when they load.
import "./stub";
import "@harness-app-css";
import React, { useEffect } from "react";
import { createRoot } from "react-dom/client";
import { frames } from "./harness";

export type PageModule = {
  default: React.ComponentType;
  /** Awaited after mount, before the capture. Whatever it returns is saved as <page>-<theme>.json. */
  setup?: () => unknown | Promise<unknown>;
  /** Viewport size in CSS px (default 1280 × 768, the GPUI window's content area). */
  size?: { width: number; height: number };
};

// Lazy: only the requested page's module (and the app code it imports) is evaluated.
// Files whose name starts with "_" are shared helpers, not pages.
const modules = import.meta.glob<PageModule>(["../pages/*.tsx", "!../pages/_*.tsx"]);
const pages = new Map(
  Object.entries(modules).map(([file, load]) => [file.replace(/^.*\/|\.tsx$/g, ""), load]),
);

export const DEFAULT_SIZE = { width: 1280, height: 768 };

// The faces globals.css imports (fontsource): UI (Inter / Mona Sans) and code (JetBrains Mono).
const FONTS = [
  ...[400, 500, 600, 700].map((w) => `${w} 14px "Inter Variable"`),
  ...[400, 500, 600, 700].map((w) => `${w} 14px "Mona Sans Variable"`),
  ...[400, 500, 600, 700].map((w) => `${w} 14px "JetBrains Mono"`),
];

declare global {
  interface Window {
    __ready: Promise<unknown>;
    __pages: string[];
    __pageSize: (name: string) => Promise<{ width: number; height: number } | null>;
  }
}

const params = new URLSearchParams(location.search);
const pageName = params.get("page") ?? "";
const theme = params.get("theme") ?? "nocturne";

try {
  localStorage.clear(); // persists across runs and loads (same file:// origin)
} catch {
  /* storage unavailable: nothing to clear */
}
document.documentElement.dataset.theme = theme;
window.__pages = [...pages.keys()].sort();
// The runner sizes the window before loading a page (a page's `size` export, else the default).
window.__pageSize = async (name) => {
  const load = pages.get(name);
  return load ? ((await load()).size ?? DEFAULT_SIZE) : null;
};

/** Waits for running CSS animations / transitions that end (entrance animations, a toggled
 *  Switch's knob), at most `capMs`. Infinite ones (spinners) are captured wherever they are. */
async function settleAnimations(capMs = 3000): Promise<void> {
  const finite = document.getAnimations().filter((a) => {
    const end = a.effect?.getComputedTiming().endTime;
    return a.playState === "running" && typeof end === "number" && Number.isFinite(end);
  });
  if (!finite.length) return;
  await Promise.race([
    Promise.all(finite.map((a) => a.finished.catch(() => undefined))),
    new Promise((r) => setTimeout(r, capMs)),
  ]);
}

function Mounted({ onMount, children }: { onMount: () => void; children: React.ReactNode }) {
  useEffect(onMount, []);
  return <>{children}</>;
}

window.__ready = (async () => {
  if (!pageName) return null; // index mode: the runner only reads __pages / __pageSize
  const load = pages.get(pageName);
  if (!load) throw new Error(`unknown page "${pageName}" (have: ${window.__pages.join(", ")})`);
  await Promise.all(FONTS.map((f) => document.fonts.load(f)));
  const mod = await load();
  const { width, height } = mod.size ?? DEFAULT_SIZE;

  const root = document.getElementById("root")!;
  for (const el of [document.documentElement, document.body, root]) {
    el.style.width = `${width}px`;
    el.style.height = `${height}px`;
  }
  root.style.position = "relative";
  root.style.overflow = "hidden";

  const Page = mod.default;
  await new Promise<void>((resolve) =>
    createRoot(root).render(
      <Mounted onMount={resolve}>
        <Page />
      </Mounted>,
    ),
  );
  await document.fonts.ready;
  await frames(2);
  await settleAnimations();
  const report = mod.setup ? await mod.setup() : undefined;
  await document.fonts.ready;
  await frames(2);
  await settleAnimations();
  await frames(2);
  return report ?? null;
})();
// Logged here too so a failure shows in the runner's console.
window.__ready.catch((e) => console.error("[harness] page failed:", e));
