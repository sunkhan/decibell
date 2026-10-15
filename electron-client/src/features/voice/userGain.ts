// Per-user playback gain: the dB ↔ linear mapping, the "locally muted"
// override, and the single place that pushes a user's effective gain to the
// native mixer. Shared by the user context menu, the DM call panel and the
// roster re-apply in useVoiceEvents so the three never drift apart.

import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { uidNamed, useUsersStore, type Uid } from "../../stores/usersStore";

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

// Saved per-user volume / local mute are keyed by uid (as decimal strings in
// settings). Settings written before uids keyed them by name: those entries
// wait here and move to their uid the first time the servers pair that name
// with one, and are written back unchanged until then.
const legacyVolumes = new Map<string, number>();
const legacyMuted = new Set<string>();

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
    else legacyVolumes.set(key, db);
  }
  for (const key of muted) {
    const uid = asUid(key);
    if (uid === null) legacyMuted.add(key);
    else if (!useVoiceStore.getState().localMutedUsers.has(uid)) store.toggleLocalMute(uid);
  }
  migrateLegacyGains();
}

/// What saveSettings writes: the uid-keyed settings plus any name-keyed ones
/// still waiting for their uid.
export function savedUserGains(): { user_volumes: Record<string, number>; local_muted_users: string[] } {
  const { userVolumes, localMutedUsers } = useVoiceStore.getState();
  const volumes: Record<string, number> = Object.fromEntries(legacyVolumes);
  for (const [uid, db] of Object.entries(userVolumes)) volumes[uid] = db;
  return {
    user_volumes: volumes,
    local_muted_users: [...legacyMuted, ...[...localMutedUsers].map(String)],
  };
}

function migrateLegacyGains(): void {
  if (legacyVolumes.size === 0 && legacyMuted.size === 0) return;
  const store = useVoiceStore.getState();
  let moved = false;
  for (const [name, db] of legacyVolumes) {
    const uid = uidNamed(name);
    if (uid === undefined) continue;
    if (!(uid in useVoiceStore.getState().userVolumes)) store.setUserVolume(uid, db);
    legacyVolumes.delete(name);
    moved = true;
  }
  for (const name of legacyMuted) {
    const uid = uidNamed(name);
    if (uid === undefined) continue;
    if (!useVoiceStore.getState().localMutedUsers.has(uid)) store.toggleLocalMute(uid);
    legacyMuted.delete(name);
    moved = true;
  }
  if (moved) {
    import("../settings/saveSettings").then(({ saveSettings }) => saveSettings());
    applySavedUserGains(useVoiceStore.getState().participants.map((p) => p.uid));
  }
}

// New name → uid pairs arrive through the users store.
useUsersStore.subscribe((s, prev) => {
  if (s.names !== prev.names) migrateLegacyGains();
});
