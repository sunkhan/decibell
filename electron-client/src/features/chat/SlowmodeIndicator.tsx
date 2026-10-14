import { useRef } from "react";
import { channelKey } from "../../lib/channelKey";
import { Icon } from "../servers/tabs/settingsUi";
import { useSlowmode, useSlowmodeStore } from "./slowmode";

/// "30s", "1m 30s", "2h": the slowmode interval, short.
function short(total: number): string {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return [h && `${h}h`, m && `${m}m`, s && `${s}s`].filter(Boolean).join(" ") || "0s";
}

/// "24 seconds", "1 minute 30 seconds": for the tooltip and screen readers.
function spoken(total: number): string {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const part = (n: number, unit: string) => (n ? `${n} ${unit}${n === 1 ? "" : "s"}` : "");
  return [part(h, "hour"), part(m, "minute"), part(s, "second")].filter(Boolean).join(" ") || "0 seconds";
}

/// "0:24", "4:59", "1:05:00": the time left.
function clock(total: number): string {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const ss = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/// Slowmode in the composer, for members it applies to: the interval
/// while you may post, a countdown after each message. A send held back
/// by it shakes the timer (ChatPanel keeps the text).
export default function SlowmodeIndicator({ serverId, channelId }: { serverId: string; channelId: string }) {
  const { interval, remaining } = useSlowmode(serverId, channelId);
  const key = channelKey(serverId, channelId);
  const nudge = useSlowmodeStore((s) => (s.nudge?.key === key ? s.nudge.n : 0));
  // Shake for nudges that arrive while shown, not for one from before.
  const seen = useRef(nudge);
  if (!interval) return null;
  const waiting = remaining > 0;
  const label = waiting
    ? `Slowmode: you can send again in ${spoken(remaining)}.`
    : `Slowmode: one message every ${spoken(interval)}.`;
  return (
    <span
      key={nudge}
      role="timer"
      aria-label={label}
      title={label}
      className={`flex h-[34px] shrink-0 items-center gap-1.5 rounded-md px-2 text-[12px] font-semibold tabular-nums transition-colors ${
        waiting ? "bg-accent-soft text-accent-bright" : "text-text-muted"
      } ${nudge !== seen.current ? "animate-[barNudge_0.36s_ease]" : ""}`}
    >
      <Icon name="hourglass" size={14} />
      {waiting ? clock(remaining) : short(interval)}
    </span>
  );
}
