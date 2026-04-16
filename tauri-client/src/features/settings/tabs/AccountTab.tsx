import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../../stores/authStore";

export default function AccountTab() {
  const username = useAuthStore((s) => s.username);

  const handleLogout = async () => {
    try {
      await invoke("logout");
    } catch (err) {
      console.error("Logout failed:", err);
    }
  };

  const initial = username ? username.charAt(0).toUpperCase() : "?";

  return (
    <div>
      {/* User card */}
      <div className="flex items-center gap-4 rounded-[10px] border border-border-divider bg-bg-light px-5 py-4">
        <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full bg-gradient-to-br from-accent to-accent-bright text-lg font-bold text-white">
          {initial}
        </div>
        <div>
          <div className="text-[14px] font-medium text-text-primary">{username}</div>
          <div className="mt-0.5 text-[12px] text-text-muted">Logged in</div>
        </div>
      </div>

      {/* Logout */}
      <button
        onClick={handleLogout}
        className="mt-5 rounded-[10px] border border-error/20 bg-error/10 px-5 py-2.5 text-[13px] font-medium text-error transition-colors hover:bg-error/20"
      >
        Log Out
      </button>
    </div>
  );
}
