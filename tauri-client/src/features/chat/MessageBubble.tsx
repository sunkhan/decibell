import type { Message } from "../../types";
import { stringToColor, stringToGradient } from "../../utils/colors";
import { useUiStore } from "../../stores/uiStore";

function parseTimestamp(ts: string): Date {
  const asEpoch = parseInt(ts, 10);
  return isNaN(asEpoch) ? new Date(ts) : new Date(asEpoch * 1000);
}

function formatTimestamp(ts: string): string {
  const date = parseTimestamp(ts);
  if (isNaN(date.getTime())) return ts;
  const now = new Date();
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  if (date.toDateString() === now.toDateString()) {
    return time;
  }

  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) {
    return `Yesterday, at ${time}`;
  }

  return `${date.toLocaleDateString()}, at ${time}`;
}

/** Check if this message should be grouped with the previous one (same sender, within 7 minutes). */
export function shouldGroup(prev: Message | undefined, curr: Message): boolean {
  if (!prev || prev.sender !== curr.sender) return false;
  const prevDate = parseTimestamp(prev.timestamp);
  const currDate = parseTimestamp(curr.timestamp);
  if (isNaN(prevDate.getTime()) || isNaN(currDate.getTime())) return false;
  return Math.abs(currDate.getTime() - prevDate.getTime()) < 7 * 60 * 1000;
}

interface Props {
  message: Message;
  grouped: boolean;
  serverId?: string | null;
}

export default function MessageBubble({ message, grouped, serverId }: Props) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  if (grouped) {
    return (
      <div className="group flex gap-3 rounded-xl px-2 py-px transition-colors hover:bg-white/[0.015]">
        {/* Spacer matching avatar width — hover timestamp */}
        <div className="flex w-[38px] shrink-0 items-baseline justify-end">
          <span className="text-[10px] font-medium leading-none text-text-muted opacity-0 group-hover:opacity-100">
            {parseTimestamp(message.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
          </span>
        </div>
        <div className="select-text min-w-0 flex-1">
          <p className="break-all text-sm leading-snug text-text-primary [overflow-wrap:anywhere]">
            {message.content}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="group flex gap-3 rounded-xl px-2 pt-2.5 pb-0.5 transition-colors hover:bg-white/[0.015] animate-[fadeUp_0.3s_ease_both]">
      {/* Avatar */}
      <div
        className="flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-lg text-[15px] font-bold text-white"
        style={{ background: stringToGradient(message.sender) }}
      >
        {message.sender.charAt(0).toUpperCase()}
      </div>

      {/* Content */}
      <div className="select-text min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span
            className="cursor-pointer text-sm font-bold hover:underline"
            style={{ color: stringToColor(message.sender) }}
            onClick={(e) => {
              const rect = e.currentTarget.getBoundingClientRect();
              openProfilePopup(message.sender, { x: rect.right + 8, y: rect.top }, serverId);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              openContextMenu(message.sender, { x: e.clientX, y: e.clientY });
            }}
          >
            {message.sender}
          </span>
          <span className="text-[11px] font-medium text-text-muted">
            {formatTimestamp(message.timestamp)}
          </span>
        </div>
        <p className="mt-0.5 break-all text-sm leading-relaxed text-text-primary [overflow-wrap:anywhere]">
          {message.content}
        </p>
      </div>
    </div>
  );
}
