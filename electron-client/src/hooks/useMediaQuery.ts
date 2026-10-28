import { useCallback, useSyncExternalStore } from "react";

/// Whether a CSS media query matches, following changes (window resizes).
/// For layouts that must render a subtree in one place or another, where a
/// pair of responsive classes would mount it twice.
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback(
    (onChange: () => void) => {
      const list = window.matchMedia(query);
      list.addEventListener("change", onChange);
      return () => list.removeEventListener("change", onChange);
    },
    [query],
  );
  return useSyncExternalStore(subscribe, () => window.matchMedia(query).matches);
}
