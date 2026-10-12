// Presence indicator (docs/superpowers/specs/2026-10-11-user-status-design.md).
// Shape carries the status as well as colour, so it reads without colour
// vision and in every theme: online a filled disc, away a crescent moon,
// do-not-disturb a disc with a bar, offline / invisible a hollow ring.
//
// Like every avatar dot in the app, the ring around the disc is a border
// painted in the surface colour behind it (`ring`) — and the cut-outs that
// make the moon, bar and hollow are painted in that same colour.

import type { DisplayStatus } from "../stores/presenceStore";
import { STATUS_LABEL, usePresence } from "../stores/presenceStore";

const COLOR: Record<DisplayStatus, string> = {
  online: "var(--color-success)",
  away: "var(--color-warning)",
  dnd: "var(--color-error)",
  offline: "var(--color-text-muted)",
  invisible: "var(--color-text-muted)",
};

interface StatusDotProps {
  status: DisplayStatus;
  /// The surface colour behind the dot (a CSS colour, usually a theme var).
  ring: string;
  /// Positioning + size — e.g. "avatar-dot absolute -bottom-px -right-px".
  className?: string;
  /// Ring width in px (the app uses 2–3).
  ringWidth?: number;
}

export function StatusDot({ status, ring, className = "", ringWidth = 2.5 }: StatusDotProps) {
  const color = COLOR[status];
  // The glyph is an SVG in a fixed 20×20 space, not nested boxes: a hole
  // laid out as `inset: 26%` of a 6px box was a fractional-pixel box that
  // Chromium snapped on its own, so the ring's centre sat up to a pixel
  // off — differently for every avatar's position. Vector geometry stays
  // centred wherever the dot lands. Cut-outs are painted in `ring`.
  return (
    <span
      role="img"
      aria-label={STATUS_LABEL[status]}
      className={`block shrink-0 rounded-full ${className}`}
      style={{ background: ring, border: `${ringWidth}px solid ${ring}` }}
    >
      <svg viewBox="0 0 20 20" className="block h-full w-full" aria-hidden="true">
        <circle cx="10" cy="10" r="10" style={{ fill: color }} />
        {status === "away" && (
          // The bite out of the top-left that turns the disc into a moon.
          <circle cx="4" cy="4" r="7" style={{ fill: ring }} />
        )}
        {status === "dnd" && (
          <rect x="3.5" y="7.5" width="13" height="5" rx="2.5" style={{ fill: ring }} />
        )}
        {(status === "offline" || status === "invisible") && (
          <circle cx="10" cy="10" r="4.5" style={{ fill: ring }} />
        )}
      </svg>
    </span>
  );
}

/// StatusDot for one user, subscribed to their presence — for lists that
/// render rows inline (a hook can't run per row there).
export function PresenceDot({
  username,
  communityOnline,
  ...dot
}: Omit<StatusDotProps, "status"> & { username: string; communityOnline?: boolean }) {
  return <StatusDot status={usePresence(username, communityOnline)} {...dot} />;
}

const PILL_TONE: Record<DisplayStatus, string> = {
  online: "bg-success/15 text-success",
  away: "bg-warning/15 text-warning",
  dnd: "bg-error/15 text-error",
  offline: "bg-text-muted/15 text-text-muted",
  invisible: "bg-text-muted/15 text-text-muted",
};

/// "● Online" / "● Away" / … — the profile cards and the DM header.
export function StatusPill({ status, className = "" }: { status: DisplayStatus; className?: string }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-[11px] font-medium ${PILL_TONE[status]} ${className}`}
    >
      <span className="h-1.5 w-1.5 rounded-full" style={{ background: COLOR[status] }} />
      {STATUS_LABEL[status]}
    </span>
  );
}
