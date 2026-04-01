import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";

export default function SettingsModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);

  if (activeModal !== "settings") return null;

  const handleToggle = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={closeModal}
    >
      <div
        className="w-[440px] rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-6 py-4">
          <h2 className="text-[15px] font-extrabold text-text-bright">Settings</h2>
          <button
            onClick={closeModal}
            className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        {/* Content */}
        <div className="px-6 py-5">
          {/* Privacy Section */}
          <h3 className="mb-3 text-[11px] font-bold uppercase tracking-[0.08em] text-text-muted">
            Privacy
          </h3>
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

        {/* Version label */}
        <div className="border-t border-border px-6 py-3">
          <p className="text-center text-[11px] text-text-muted">Decibell 0.1.11</p>
        </div>
      </div>
    </div>,
    document.body
  );
}
