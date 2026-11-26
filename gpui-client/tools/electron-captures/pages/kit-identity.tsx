// The GPUI kit's `kit-identity`: LetterAvatar, StatusDot, StatusPill, BellMark, ErrorCard,
// AuditLogTab's EmptyState, the banners and the notice line.
import { LetterAvatar } from "@app/components/LetterAvatar";
import { StatusDot, StatusPill } from "@app/components/StatusDot";
import { BellMark } from "@app/components/BellMark";
import ErrorCard from "@app/components/ErrorCard";
import { EmptyState } from "@app/features/servers/tabs/AuditLogTab";
import { Icon } from "@app/features/servers/tabs/settingsUi";
import { Page, Row, Section } from "./_frame";

const PEOPLE: [number, string][] = [
  [1, "aurora"], [2, "basil"], [3, "cedar"], [4, "dahlia"], [5, "ember"], [6, "fern"], [7, "gale"], [0, "guest"],
];
const STATUSES = ["online", "away", "dnd", "offline"] as const;
const RING = "var(--color-bg-primary)";

export default function KitIdentity() {
  return (
    <Page className="flex-row gap-8">
      <div className="flex w-[600px] flex-col gap-6">
        <Section title="Letter avatars · 16 20 24 32 40 48 80">
          <Row className="items-end">{[16, 20, 24, 32, 40, 48, 80].map((s) => <LetterAvatar key={s} uid={1} name="aurora" size={s} />)}</Row>
        </Section>
        <Section title="Colours by uid (0: by name)">
          <Row>{PEOPLE.map(([uid, name]) => <LetterAvatar key={name} uid={uid} name={name} size={32} />)}</Row>
        </Section>
        <Section title="With presence · online, away, do not disturb, offline">
          <Row className="gap-4">
            {STATUSES.map((status, i) => (
              <div key={status} className="relative">
                <LetterAvatar uid={PEOPLE[i][0]} name={PEOPLE[i][1]} size={40} />
                <StatusDot status={status} ring={RING} className="avatar-dot absolute -bottom-px -right-px" />
              </div>
            ))}
          </Row>
        </Section>
        <Section title="Status dots · 10 13 16 24">
          <Row>
            {[10, 13, 16, 24].map((s) => (
              <Row key={s} className="gap-1">
                {STATUSES.map((status) => <StatusDot key={status} status={status} ring={RING} className="" ringWidth={2.5} />).map((dot, i) => (
                  <span key={i} style={{ width: s, height: s }} className="flex [&>span]:h-full [&>span]:w-full">{dot}</span>
                ))}
              </Row>
            ))}
          </Row>
        </Section>
        <Section title="Status pills">
          <Row>{STATUSES.map((status) => <StatusPill key={status} status={status} />)}</Row>
        </Section>
        <Section title="Bell mark">
          <Row className="items-end gap-8">
            <BellMark />
            <BellMark height={56} barWidth={5} gap={3} />
          </Row>
        </Section>
      </div>
      <div className="flex w-[580px] flex-col gap-6">
        <Section title="Error card · notice">
          <div className="flex flex-col gap-3">
            <ErrorCard>Couldn't reach Daily Gaming. Check your connection; Decibell keeps trying.</ErrorCard>
            <div className="flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3 py-2.5 text-[13px] text-error">
              <Icon name="alert" size={15} />
              You don't have permission to send messages here.
            </div>
          </div>
        </Section>
        <Section title="Banners">
          <div className="flex flex-col gap-2">
            <div className="flex h-8 shrink-0 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">Connection lost. Reconnecting...</div>
            <div className="flex h-9 shrink-0 items-center justify-between border-b border-border-divider bg-accent-soft px-4 text-[12px] text-accent-bright">
              <span>Decibell sends anonymous crash reports to help us fix bugs.</span>
              <button className="ml-3 flex h-6 w-6 shrink-0 items-center justify-center rounded-sm text-accent-bright transition-colors hover:bg-accent-mid">
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </div>
          </div>
        </Section>
        <Section title="Empty state">
          <EmptyState icon="ban" title="No bans" body="Members you ban show up here, with who banned them and why." />
        </Section>
      </div>
    </Page>
  );
}
