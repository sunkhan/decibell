import { useEffect, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useE2eeStore } from "../../stores/e2eeStore";
import type { E2eePeerInfo } from "../../types";

// A peer's encryption state as native sees it (keys published? their
// fingerprint, the conversation's safety number). Re-fetched when our own
// status changes (locked → ready pairs a local identity with theirs) and
// when native reports the peer's identity changed. Native caches the
// bundle per session, so repeated calls are cheap.
export function usePeerE2ee(peer: string | null | undefined): E2eePeerInfo | null {
  const status = useE2eeStore((s) => s.status);
  const changedAt = useE2eeStore((s) => (peer ? s.changedPeers[peer] ?? 0 : 0));
  const [info, setInfo] = useState<E2eePeerInfo | null>(null);

  useEffect(() => {
    setInfo(null);
    if (!peer || status === "unavailable") return;
    let cancelled = false;
    invoke<E2eePeerInfo>("e2ee_peer_info", { username: peer })
      .then((i) => {
        if (!cancelled) setInfo(i);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [peer, status, changedAt]);

  return info;
}
