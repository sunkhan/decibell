import { useUiStore } from "../stores/uiStore";
import { saveSettings } from "../features/settings/saveSettings";

// First-launch disclosure for crash reporting. Mounted in MainLayout
// (post-login chrome). Shown until the user dismisses; never reappears
// after that. No "decline" button — opt-out lives in Settings →
// Privacy. The toggle in PrivacyTab can revert the user's choice;
// this banner is a one-way disclosure.
export default function CrashReportingBanner() {
  const shown = useUiStore((s) => s.crashReportingConsentShown);
  if (shown) return null;

  const dismiss = () => {
    useUiStore.getState().setCrashReportingConsentShown(true);
    saveSettings();
  };

  return (
    <div className="flex h-9 shrink-0 items-center justify-between border-b border-border-divider bg-accent-soft px-4 text-[12px] text-accent-bright">
      <span>
        Decibell sends anonymous crash reports to help us fix bugs. You can
        disable this in Settings → Privacy.
      </span>
      <button
        onClick={dismiss}
        className="ml-3 flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-accent-bright transition-colors hover:bg-accent-mid"
        title="Dismiss"
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
        >
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}
