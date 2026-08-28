// Per-user playback gain: the dB ↔ linear mapping, the "locally muted"
// override, and the single place that pushes a user's effective gain to the
// native mixer. Shared by the user context menu, the DM call panel and the
// roster re-apply in useVoiceEvents so the three never drift apart.

import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";

export const MIN_DB = -40;
export const MAX_DB = 15;
export const DEFAULT_DB = 0;

/// dB → linear gain. The -40 dB floor maps to 0 (effectively muted).
export function dbToGain(db: number): number {
  if (db <= MIN_DB) return 0;
  return Math.pow(10, db / 20);
}

export function formatDb(db: number): string {
  if (db <= MIN_DB) return "Muted";
  if (db === 0) return "0 dB";
  return `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`;
}

export function dbToPercent(db: number): string {
  if (db <= MIN_DB) return "0%";
  return `${Math.round(dbToGain(db) * 100)}%`;
}

/// True while a native VoiceEngine exists to receive gain updates — a
/// community voice channel OR a P2P DM call (same engine, no channel).
export function mediaSessionActive(): boolean {
  const v = useVoiceStore.getState();
  return v.connectedChannelId !== null || v.callPeer !== null;
}

/// The gain `username` should currently play at: 0 when locally muted,
/// else their saved dB (default 0 dB).
export function effectiveGain(username: string): number {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  if (localMutedUsers.has(username)) return 0;
  return dbToGain(userVolumes[username] ?? DEFAULT_DB);
}

/// Push `username`'s effective gain to native. No-op without a session:
/// the store still persists the change and the next session start replays
/// it (`applySavedUserGains`).
export function pushUserGain(username: string): void {
  if (!mediaSessionActive()) return;
  invoke("set_user_volume", { username, gain: effectiveGain(username) }).catch(console.error);
}

/// Replay saved volume / local-mute for every listed user — on a voice
/// roster change and when a DM call connects, so a peer picked up after
/// they were muted stays muted and volume tweaks survive churn.
export function applySavedUserGains(usernames: Iterable<string>): void {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  for (const user of usernames) {
    if (user in userVolumes || localMutedUsers.has(user)) pushUserGain(user);
  }
}
