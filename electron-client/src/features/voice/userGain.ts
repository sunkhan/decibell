// Per-user playback gain: the dB ↔ linear mapping, the "locally muted"
// override, and the single place that pushes a user's effective gain to the
// native mixer. Shared by the user context menu, the DM call panel and the
// roster re-apply in useVoiceEvents so the three never drift apart.

import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import type { Uid } from "../../stores/usersStore";

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

/// The gain `uid` should currently play at: 0 when locally muted, else
/// their saved dB (default 0 dB).
export function effectiveGain(uid: Uid): number {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  if (localMutedUsers.has(uid)) return 0;
  return dbToGain(userVolumes[uid] ?? DEFAULT_DB);
}

/// Push `uid`'s effective gain to native (the media plane names speakers by
/// uid). No-op without a session: the store still persists the change and
/// the next session start replays it (`applySavedUserGains`).
export function pushUserGain(uid: Uid): void {
  if (!mediaSessionActive() || !(uid > 0)) return;
  invoke("set_user_volume", { uid, gain: effectiveGain(uid) }).catch(console.error);
}

/// Replay saved volume / local-mute for every listed user — on a voice
/// roster change and when a DM call connects, so a peer picked up after
/// they were muted stays muted and volume tweaks survive churn.
export function applySavedUserGains(uids: Iterable<Uid>): void {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  for (const uid of uids) {
    if (uid in userVolumes || localMutedUsers.has(uid)) pushUserGain(uid);
  }
}

// Saved per-user volume / local mute are keyed by uid (decimal strings in
// settings: user_volumes_by_uid / local_muted_uids).

function asUid(key: string): Uid | null {
  const n = Number(key);
  return Number.isSafeInteger(n) && n > 0 && String(n) === key ? n : null;
}

/// Restore the saved per-user settings (loadSettings).
export function restoreSavedUserGains(volumes: Record<string, number>, muted: string[]): void {
  const store = useVoiceStore.getState();
  for (const [key, db] of Object.entries(volumes)) {
    const uid = asUid(key);
    if (uid !== null) store.setUserVolume(uid, db);
  }
  for (const key of muted) {
    const uid = asUid(key);
    if (uid !== null && !useVoiceStore.getState().localMutedUsers.has(uid)) store.toggleLocalMute(uid);
  }
}

/// What saveSettings writes.
export function savedUserGains(): { user_volumes_by_uid: Record<string, number>; local_muted_uids: string[] } {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  return {
    user_volumes_by_uid: Object.fromEntries(Object.entries(userVolumes)),
    local_muted_uids: [...localMutedUsers].map(String),
  };
}
