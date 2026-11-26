// The GPUI kit's `kit-buttons` (crates/decibell-ui/src/kit.rs): the renderer's button class strings.
import { Icon } from "@app/features/servers/tabs/settingsUi";
import { Page, Row, Section } from "./_frame";

const MD = "px-4 py-2 text-[13px]";
const SM = "px-3 py-1.5 text-[12px]";
const VARIANTS: [string, (size: string) => string][] = [
  ["Save changes", (s) => `rounded-sm bg-accent ${s} font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50`],
  ["Cancel", (s) => `rounded-md border border-border bg-transparent ${s} font-medium text-text-primary transition-colors hover:bg-surface-hover`],
  ["Reset", (s) => `rounded-sm ${s} font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-50`],
  ["Delete", (s) => `rounded-md bg-error ${s} font-semibold text-on-error transition-colors hover:bg-error/90 disabled:opacity-50`],
  ["Remove", (s) => `rounded-md border border-error/20 bg-error/10 ${s} font-medium text-error transition-colors hover:bg-error/20 disabled:opacity-50`],
  ["Leave", (s) => `rounded-sm border border-error/40 ${s} font-semibold text-error hover:border-error/70 hover:bg-error/10`],
  ["Copy invite", (s) => `rounded-sm bg-accent-soft ${s} font-medium text-accent-bright transition-colors hover:bg-accent-mid`],
  // KeybindsTab's Record again / AudioTab's Test mic, E2eeSection's SecondaryButton.
  ["Record again", (s) => `rounded-sm border border-border ${s} font-medium text-text-secondary transition-colors hover:border-accent/40 hover:text-text-primary`],
  ["Reset keys", (s) => `rounded-sm border border-border ${s} font-medium text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary`],
];

/// features/auth/LoginPage.tsx's spinner, held still (the GPUI capture draws it at rest).
const Spin = ({ className }: { className: string }) => (
  <svg className={className} viewBox="0 0 24 24" fill="none" style={{ animation: "none" }}>
    <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" className="opacity-25" />
    <path d="M4 12a8 8 0 018-8" stroke="currentColor" strokeWidth="3" strokeLinecap="round" className="opacity-75" />
  </svg>
);

const Close = ({ size }: { size: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

export default function KitButtons() {
  return (
    <Page>
      <Section title="Buttons · md">
        <Row>{VARIANTS.map(([label, cls]) => <button key={label} className={cls(MD)}>{label}</button>)}</Row>
      </Section>
      <Section title="Buttons · sm">
        <Row>{VARIANTS.map(([label, cls]) => <button key={label} className={cls(SM)}>{label}</button>)}</Row>
      </Section>
      <Section title="Sizes · lg, xs, rounded-sm">
        <Row>
          {/* AccountTab's Log Out, AudioTab's Test mic, DevicesTab's Sign out */}
          <button className="rounded-md border border-error/20 bg-error/10 px-5 py-2.5 text-[13px] font-medium text-error transition-colors hover:bg-error/20">Log Out</button>
          <button className="rounded-sm border border-border px-2 py-0.5 text-[12px] font-medium text-text-secondary transition-colors hover:border-accent/40 hover:text-text-primary">Test mic</button>
          <button className="shrink-0 rounded-sm border border-error/20 bg-error/10 px-3 py-1.5 text-[12px] font-medium text-error transition-colors hover:bg-error/20 disabled:opacity-50">Sign out</button>
        </Row>
      </Section>
      <Section title="States · disabled, loading, icon">
        <Row>
          <button disabled className={VARIANTS[0][1](MD)}>Save changes</button>
          <button disabled className={VARIANTS[3][1](MD)}>Delete</button>
          <button disabled className={`inline-flex items-center gap-1.5 ${VARIANTS[0][1](MD)}`}>
            <Spin className="h-3.5 w-3.5" />
            Saving…
          </button>
          <button className={`inline-flex items-center gap-1.5 ${VARIANTS[0][1](MD)}`}>
            <Icon name="plus" size={14} />
            Add role
          </button>
        </Row>
      </Section>
      <Section title="Full width">
        <div className="w-80">
          <button className={`flex w-full justify-center ${VARIANTS[0][1](MD)}`}>Log In</button>
        </div>
      </Section>
      <Section title="Icon buttons · links">
        <Row>
          <button className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
            <Close size={14} />
          </button>
          <button className="flex h-6 w-6 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
            <Close size={12} />
          </button>
          <button className="text-[12px] font-semibold text-accent-bright underline-offset-2 hover:underline">Undo</button>
          <button className="text-[11px] font-medium text-accent-bright transition-colors hover:text-accent">Reset</button>
        </Row>
      </Section>
      <Section title="Spinners">
        <Row>
          <Spin className="h-4 w-4 text-accent" />
          <Spin className="h-5 w-5 text-text-primary" />
          <Spin className="h-8 w-8 text-accent" />
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" className="text-text-secondary">
            <circle cx="12" cy="12" r="9" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
            <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
          </svg>
        </Row>
      </Section>
      <Section title="Badges">
        <Row>
          <span className="rounded-sm bg-accent-soft px-1.5 py-px text-[11px] font-semibold text-accent-bright">New</span>
          <span className="rounded-sm bg-accent-mid px-1.5 py-0.5 text-[11px] font-semibold text-accent-bright">this window</span>
          <span className="rounded-sm bg-error/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-error">Blocked</span>
          <span className="rounded-sm bg-accent-soft px-2 py-0.5 text-[12px] font-medium text-accent-bright">120 ms</span>
        </Row>
      </Section>
    </Page>
  );
}
