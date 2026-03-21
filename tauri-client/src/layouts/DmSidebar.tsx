import { invoke } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";
import { useChatStore } from "../stores/chatStore";
import { useUiStore } from "../stores/uiStore";
import { useAuthStore } from "../stores/authStore";

export default function DmSidebar() {
  const navigate = useNavigate();
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const activeView = useUiStore((s) => s.activeView);

  const handleLogout = async () => {
    try {
      await invoke("logout");
    } catch (err) {
      console.error("Logout failed:", err);
    }
    useAuthStore.getState().logout();
    navigate("/login");
  };

  const handleHomeClick = () => {
    setActiveServer(null);
    setActiveChannel(null);
    setActiveView("home");
  };

  return (
    <div className="flex h-full w-[72px] flex-shrink-0 flex-col items-center border-r border-border bg-bg-primary pt-3">
      <button
        onClick={handleHomeClick}
        className={`flex h-12 w-12 items-center justify-center rounded-xl transition-colors ${
          activeView === "home"
            ? "bg-accent text-white"
            : "bg-bg-tertiary text-text-muted hover:bg-white/10"
        }`}
        title="Home"
      >
        <svg className="h-5 w-5" fill="currentColor" viewBox="0 0 20 20">
          <path d="M10.707 2.293a1 1 0 00-1.414 0l-7 7a1 1 0 001.414 1.414L4 10.414V17a1 1 0 001 1h2a1 1 0 001-1v-2a1 1 0 011-1h2a1 1 0 011 1v2a1 1 0 001 1h2a1 1 0 001-1v-6.586l.293.293a1 1 0 001.414-1.414l-7-7z" />
        </svg>
      </button>
      <div className="my-2 h-px w-8 bg-border" />
      <div className="flex flex-1 flex-col items-center gap-2 overflow-y-auto py-1" />
      <button
        onClick={handleLogout}
        className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-bg-tertiary text-text-muted transition-colors hover:bg-error/20 hover:text-error"
        title="Log out"
      >
        <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
          <path strokeLinecap="round" strokeLinejoin="round" d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
        </svg>
      </button>
    </div>
  );
}
