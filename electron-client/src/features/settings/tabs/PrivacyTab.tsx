import { invoke } from "../../../lib/ipc";
import { useDmStore } from "../../../stores/dmStore";
import { useUiStore } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";

export default function PrivacyTab() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);
  const crashReportingEnabled = useUiStore((s) => s.crashReportingEnabled);

  const handleToggleDms = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
    saveSettings();
  };

  const handleToggleCrashReporting = () => {
    const next = !crashReportingEnabled;
    useUiStore.getState().setCrashReportingEnabled(next);
    saveSettings();
    // No live SDK teardown — takes effect on next launch.
  };

  return (
    <div className="flex flex-col gap-3">
      <ToggleRow
        title="Only accept DMs from friends"
        description="When enabled, only users in your friends list can send you direct messages"
        value={friendsOnlyDms}
        onToggle={handleToggleDms}
      />
      <ToggleRow
        title="Send anonymous crash reports"
        description="Helps fix bugs that happen in the field. No usernames, no message contents, no server names. Restart required for changes to apply."
        value={crashReportingEnabled}
        onToggle={handleToggleCrashReporting}
      />
    </div>
  );
}

function ToggleRow({
  title,
  description,
  value,
  onToggle,
}: {
  title: string;
  description: string;
  value: boolean;
  onToggle: () => void;
}) {
  return (
    <div className="flex items-center justify-between rounded-[10px] border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
      <div className="pr-4">
        <div className="text-[14px] font-medium text-text-primary">{title}</div>
        <div className="mt-1 text-[12px] leading-relaxed text-text-muted">
          {description}
        </div>
      </div>
      <button
        onClick={onToggle}
        className={`relative h-[22px] w-[40px] shrink-0 rounded-full border transition-all ${
          value
            ? "border-accent bg-accent shadow-[0_0_8px_rgba(56,143,255,0.22)]"
            : "border-border bg-bg-lighter"
        }`}
      >
        <div
          className={`absolute top-[3px] h-[16px] w-[16px] rounded-full transition-all ${
            value ? "translate-x-[18px] bg-white" : "translate-x-[3px] bg-text-muted"
          }`}
        />
      </button>
    </div>
  );
}
