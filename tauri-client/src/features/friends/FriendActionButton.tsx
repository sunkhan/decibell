import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Maps to protobuf FriendActionType enum
const FRIEND_ACTIONS = {
  ADD: 0,
  REMOVE: 1,
  BLOCK: 2,
  ACCEPT: 3,
  REJECT: 4,
} as const;

interface Props {
  action: keyof typeof FRIEND_ACTIONS;
  targetUsername: string;
  label: string;
  variant?: "accent" | "success" | "error" | "muted";
}

export default function FriendActionButton({
  action,
  targetUsername,
  label,
  variant = "accent",
}: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleClick = async () => {
    setLoading(true);
    setError(null);
    try {
      await invoke("send_friend_action", {
        action: FRIEND_ACTIONS[action],
        targetUsername,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const colorClass = {
    accent: "bg-accent hover:bg-accent-hover",
    success: "bg-success hover:bg-success/80",
    error: "bg-error hover:bg-error/80",
    muted: "bg-bg-tertiary hover:bg-white/10",
  }[variant];

  return (
    <div className="inline-flex flex-col">
      <button
        onClick={handleClick}
        disabled={loading}
        className={`rounded-md px-2.5 py-1 text-xs font-semibold text-white transition-colors disabled:opacity-50 ${colorClass}`}
      >
        {loading ? "..." : label}
      </button>
      {error && <span className="mt-0.5 text-[10px] text-error">{error}</span>}
    </div>
  );
}
