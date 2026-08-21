/// Stable empty collections for zustand selector fallbacks.
///
/// `useStore((s) => s.thingsByKey[key] ?? [])` creates a NEW array on every
/// call while the key is absent. useSyncExternalStore compares snapshots
/// with Object.is, sees a different value each time, re-renders, and loops
/// ("The result of getSnapshot should be cached") — a blank screen at
/// startup. Fall back to these shared instances instead, and derive
/// filtered/mapped views with useMemo outside the selector.
export const EMPTY_LIST: never[] = [];
