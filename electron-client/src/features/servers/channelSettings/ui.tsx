import type { ReactNode } from "react";
import { Icon, SectionLabel, type IconName } from "../tabs/settingsUi";
import type { ChannelInfo } from "../../../types";

/// Building blocks shared by the channel-settings tabs. Same vocabulary as
/// the server-settings tabs: group labels over bordered cards of rows, a
/// 40px field, a notice banner.

export const FIELD =
  "w-full rounded-md border border-border bg-bg-lighter text-[13px] text-text-primary outline-none transition-all placeholder:text-text-muted hover:border-accent/40 focus:border-accent focus:shadow-ring disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:border-border";

export function channelIcon(type: ChannelInfo["type"]): IconName {
  return type === "voice" ? "speaker" : type === "category" ? "folder" : "hash";
}

export function Field({
  label,
  aside,
  hint,
  children,
}: {
  label: string;
  aside?: ReactNode;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div>
      <div className="mb-2.5 flex h-3 items-center justify-between gap-3">
        <SectionLabel>{label}</SectionLabel>
        {aside}
      </div>
      {children}
      {hint && <div className="mt-2 text-[12px] leading-[1.55] text-text-muted">{hint}</div>}
    </div>
  );
}

/// A labelled card of rows split by hairlines.
export function Group({ label, aside, children }: { label: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <div>
      <div className="mb-2.5 flex h-3 items-center justify-between gap-3">
        <SectionLabel>{label}</SectionLabel>
        {aside}
      </div>
      <div className="divide-y divide-border-divider overflow-hidden rounded-lg border border-border-divider bg-bg-light">
        {children}
      </div>
    </div>
  );
}

/// One setting in a Group: icon tile, title and description, the control
/// on the right. `below` spans the full width under the text (a slider).
export function Row({
  icon,
  tone = "neutral",
  title,
  description,
  control,
  below,
}: {
  icon?: IconName;
  tone?: "neutral" | "good" | "danger";
  title: ReactNode;
  description?: ReactNode;
  control?: ReactNode;
  below?: ReactNode;
}) {
  return (
    <div className="px-4 py-3.5">
      <div className="flex items-center gap-3.5">
        {icon && (
          <span
            className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-md ${
              tone === "good"
                ? "bg-success/15 text-success"
                : tone === "danger"
                  ? "bg-error/10 text-error"
                  : "bg-bg-lighter text-text-secondary"
            }`}
          >
            <Icon name={icon} size={17} />
          </span>
        )}
        <div className="min-w-0 flex-1">
          <div className="text-[14px] font-medium text-text-primary">{title}</div>
          {description && (
            <div className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">{description}</div>
          )}
        </div>
        {control}
      </div>
      {below}
    </div>
  );
}

export function Notice({ icon, children }: { icon: IconName; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2.5 rounded-md border border-border-divider bg-bg-light px-3.5 py-3 text-[13px] leading-[1.55] text-text-secondary">
      <Icon name={icon} size={15} className="text-text-muted" />
      <span className="min-w-0 flex-1">{children}</span>
    </div>
  );
}

/// A small status pill for read-only values (encryption).
export function Pill({ tone, icon, children }: { tone: "good" | "muted"; icon?: IconName; children: ReactNode }) {
  return (
    <span
      className={`inline-flex h-6 shrink-0 items-center gap-1.5 rounded-sm px-2 text-[12px] font-semibold ${
        tone === "good" ? "bg-success/15 text-success" : "bg-surface-active text-text-secondary"
      }`}
    >
      {icon && <Icon name={icon} size={12} strokeWidth={2.5} />}
      {children}
    </span>
  );
}
