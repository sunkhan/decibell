import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

/// How long a save waits for the server's echo before handing the draft
/// back for another try.
export const ECHO_WAIT_MS = 5000;

/// A settings save that settles on the server's echo, not on the request
/// going out. Dropping a draft as soon as the request is sent flicks the
/// fields back to the old values for a round trip, and loses an edit the
/// server then refuses. So the draft stays and the save bar reads
/// "Saving…" until one of:
///  - the store shows what was sent (`isEchoed`): `onEcho` runs, where the
///    caller drops its draft;
///  - the server refuses: the caller calls `stop()`;
///  - ECHO_WAIT_MS passes.
/// The last two leave the draft in place.
export function useEchoSave<T>(isEchoed: (sent: T) => boolean, onEcho: (sent: T) => void) {
  const [pending, setPending] = useState<{ sent: T } | null>(null);
  const echoed = !!pending && isEchoed(pending.sent);
  const onEchoRef = useRef(onEcho);
  useLayoutEffect(() => {
    onEchoRef.current = onEcho;
  });

  useEffect(() => {
    if (!pending) return;
    if (echoed) {
      onEchoRef.current(pending.sent);
      setPending(null);
      return;
    }
    const t = setTimeout(() => setPending(null), ECHO_WAIT_MS);
    return () => clearTimeout(t);
  }, [pending, echoed]);

  const start = useCallback((sent: T) => setPending({ sent }), []);
  const stop = useCallback(() => setPending(null), []);
  return { saving: !!pending, sent: pending ? pending.sent : null, start, stop };
}
