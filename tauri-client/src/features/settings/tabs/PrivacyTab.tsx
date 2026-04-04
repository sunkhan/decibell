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
      <div className="flex items-center justify-between rounded-xl bg-bg-primary px-4 py-3">
        <div>
          <div className="text-[13px] font-semibold text-text-primary">
            Only accept DMs from friends
          </div>
          <div className="mt-0.5 text-[11px] text-text-muted">
            When enabled, only users in your friends list can send you direct messages
          </div>
        </div>
        <button
          onClick={handleToggle}
          className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
            friendsOnlyDms ? "bg-accent" : "bg-text-muted/30"
          }`}
        >
          <div
            className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
              friendsOnlyDms ? "translate-x-[22px]" : "translate-x-0.5"
            }`}
          />
        </button>
      </div>
    </div>
  );
}
