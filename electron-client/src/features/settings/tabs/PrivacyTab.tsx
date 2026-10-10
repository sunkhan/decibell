import { invoke } from "../../../lib/ipc";
import { useDmStore } from "../../../stores/dmStore";
import { useUiStore } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";
import Switch from "../../../components/Switch";
import E2eeSection from "../../e2ee/E2eeSection";

export default function PrivacyTab() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);
  const crashReportingEnabled = useUiStore((s) => s.crashReportingEnabled);
  const linkPreviewsEnabled = useUiStore((s) => s.linkPreviewsEnabled);
  const gifUnfiltered = useUiStore((s) => s.gifUnfiltered);

  const handleToggleDms = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
    saveSettings();
  };

  const handleToggleLinkPreviews = () => {
    useUiStore.getState().setLinkPreviewsEnabled(!linkPreviewsEnabled);
    saveSettings();
  };

  const handleToggleGifUnfiltered = () => {
    useUiStore.getState().setGifUnfiltered(!gifUnfiltered);
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
      <E2eeSection />
      <ToggleRow
        title="Only accept DMs from friends"
        description="When enabled, only users in your friends list can send you direct messages"
        value={friendsOnlyDms}
        onToggle={handleToggleDms}
      />
      <ToggleRow
        title="Show link previews"
        description="Fetches a title, description and thumbnail for links in messages. Each site you preview sees your IP address, the same as opening the link would."
        value={linkPreviewsEnabled}
        onToggle={handleToggleLinkPreviews}
      />
      <ToggleRow
        title="Unfiltered GIF search"
        description="Search GIFs with the provider's content filter switched off, explicit content included. Off hides explicit adult content and nothing else."
        value={gifUnfiltered}
        onToggle={handleToggleGifUnfiltered}
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
    <div className="flex items-center justify-between rounded-md border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
      <div className="pr-4">
        <div className="text-[14px] font-medium text-text-primary">{title}</div>
        <div className="mt-1 text-[12px] leading-[1.55] text-text-muted">
          {description}
        </div>
      </div>
      <Switch checked={value} onToggle={onToggle} label={title} />
    </div>
  );
}
