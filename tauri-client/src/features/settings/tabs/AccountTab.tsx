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
      <div className="flex items-center gap-3.5 rounded-xl bg-bg-primary px-5 py-4">
        <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full bg-gradient-to-br from-accent to-accent-bright text-lg font-bold text-white">
          {initial}
        </div>
        <div>
          <div className="text-sm font-bold text-text-bright">{username}</div>
          <div className="mt-0.5 text-[11px] text-text-muted">Logged in</div>
        </div>
      </div>

      {/* Logout */}
      <button
        onClick={handleLogout}
        className="mt-5 rounded-lg border border-danger/20 bg-danger/10 px-5 py-2 text-[13px] font-semibold text-danger transition-colors hover:bg-danger/20"
      >
        Log Out
      </button>
    </div>
  );
}
