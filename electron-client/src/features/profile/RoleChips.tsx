import type { ServerRole } from "../../types";

/// The role chips a member's profile shows: Owner first, then their roles
/// resolved against the server's role list, or a plain "Member" when they
/// hold none (or the server predates roles). Shared by the anchored popup
/// and the full profile screen so the two never drift.
export function RoleChips({
  roles,
  roleIds,
  isOwner,
}: {
  roles: ServerRole[] | undefined;
  roleIds: ReadonlyArray<number>;
  isOwner: boolean;
}) {
  const chips = roleIds
    .map((id) => roles?.find((r) => r.id === id))
    .filter((r): r is ServerRole => !!r);
  return (
    <div className="flex flex-wrap gap-1.5">
      {isOwner && (
        <Chip color="var(--color-warning)" label="Owner" />
      )}
      {chips.map((r) => (
        <Chip
          key={r.id}
          color={r.color ? `#${r.color.toString(16).padStart(6, "0")}` : "var(--color-text-muted)"}
          label={r.name}
        />
      ))}
      {!isOwner && chips.length === 0 && (
        <Chip color="var(--color-accent-bright)" label="Member" />
      )}
    </div>
  );
}

function Chip({ color, label }: { color: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-[5px] rounded-sm border border-border-divider bg-bg-lighter px-2 py-[3px] text-[11px] font-medium text-text-secondary">
      <span className="h-2 w-2 rounded-full" style={{ backgroundColor: color }} />
      {label}
    </span>
  );
}
