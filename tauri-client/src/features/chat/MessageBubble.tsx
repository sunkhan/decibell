import type { Message } from "../../types";

// Same color function as server discovery (will be extracted in Task 10)
function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

function formatTimestamp(ts: string): string {
  const asEpoch = parseInt(ts, 10);
  const date = isNaN(asEpoch) ? new Date(ts) : new Date(asEpoch * 1000);
  if (isNaN(date.getTime())) return ts;
  const now = new Date();
  const isToday = date.toDateString() === now.toDateString();
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return isToday ? `Today at ${time}` : `${date.toLocaleDateString()} ${time}`;
}

export default function MessageBubble({ message }: { message: Message }) {
  return (
    <div className="flex gap-3 px-4 py-1.5 hover:bg-white/[0.02]">
      {/* Avatar */}
      <div
        className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-xl text-sm font-bold text-white"
        style={{ backgroundColor: stringToColor(message.sender) }}
      >
        {message.sender.charAt(0).toUpperCase()}
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="text-sm font-semibold text-accent">
            {message.sender}
          </span>
          <span className="text-[11px] text-text-muted">
            {formatTimestamp(message.timestamp)}
          </span>
        </div>
        <p className="mt-0.5 break-words text-sm text-text-primary">
          {message.content}
        </p>
      </div>
    </div>
  );
}
