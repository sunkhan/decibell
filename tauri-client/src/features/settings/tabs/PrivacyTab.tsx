import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../../../stores/dmStore";
import { saveSettings } from "../saveSettings";

export default function PrivacyTab() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);

  const handleToggle = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
    saveSettings();
  };

  return (
    <div>
      <div className="flex items-center justify-between rounded-[10px] border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
        <div className="pr-4">
          <div className="text-[14px] font-medium text-text-primary">
            Only accept DMs from friends
          </div>
          <div className="mt-1 text-[12px] leading-relaxed text-text-muted">
            When enabled, only users in your friends list can send you direct messages
          </div>
        </div>
        <button
          onClick={handleToggle}
          className={`relative h-[22px] w-[40px] shrink-0 rounded-full border transition-all ${
            friendsOnlyDms
              ? "border-accent bg-accent shadow-[0_0_8px_rgba(56,143,255,0.22)]"
              : "border-border bg-bg-lighter"
          }`}
        >
          <div
            className={`absolute top-[3px] h-[16px] w-[16px] rounded-full transition-all ${
              friendsOnlyDms
                ? "translate-x-[18px] bg-white"
                : "translate-x-[3px] bg-text-muted"
            }`}
          />
        </button>
      </div>
    </div>
  );
}
