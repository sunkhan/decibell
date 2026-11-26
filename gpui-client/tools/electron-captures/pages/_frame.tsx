// Shared by the kit pages. A kit screen in the GPUI capture is the window's content under its
// 32 px title bar. Pages whose overlays place themselves against the whole window (modals,
// toasts, menus clamped to the window) are drawn as the whole window instead: `Framed` puts a
// title-bar band above the content and the page exports `size` (1280 × 800, or 900 × 800) and
// `setup` returning `{ framed: true }`, which pairs.mjs reads to crop the band off.
import type { ReactNode } from "react";

export const TITLEBAR = 32;

export function Framed({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-full w-full flex-col bg-bg-primary text-text-primary">
      <div className="h-8 shrink-0 border-b border-border bg-bg-titlebar" />
      <div className="relative min-h-0 flex-1">{children}</div>
    </div>
  );
}

/// A titled group, as the GPUI kit's `section`: `flex flex-col gap-2`, the title 12 px muted.
export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <div className="text-[12px] text-text-muted">{title}</div>
      {children}
    </div>
  );
}

/// The GPUI kit's `row`: `flex flex-wrap items-center gap-3` (a gap in `className` replaces the
/// 3: two gap utilities on one element resolve by stylesheet order, not by the order written).
export function Row({ children, className = "" }: { children: ReactNode; className?: string }) {
  const gap = /(^|\s)gap-/.test(className) ? "" : "gap-3";
  return <div className={`flex flex-wrap items-center ${gap} ${className}`}>{children}</div>;
}

/// The GPUI kit's `page`: `p-6 flex flex-col gap-6` on bg-primary.
export function Page({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`relative flex h-full w-full flex-col gap-6 bg-bg-primary p-6 text-text-primary ${className}`}>{children}</div>;
}
