// Tauri-compatible IPC surface: invoke(method, args) and listen(name, handler)
// have the exact signatures of @tauri-apps/api/core and @tauri-apps/api/event,
// so the 117 callsites and 60 files importing those modules in the
// reference tauri-client/ port over with a single regex sweep:
//
//   import { invoke } from "@tauri-apps/api/core";    →  import { invoke } from "@/lib/ipc";
//   import { listen } from "@tauri-apps/api/event";   →  import { listen } from "@/lib/ipc";
//
// The preload bridge in electron/preload/index.ts is what actually
// implements window.decibell — see src/types/global.d.ts for the type.

export interface Event<T> {
  event: string;
  payload: T;
  id: number;
}

export type UnlistenFn = () => void;

// napi-rs auto-camelCases #[napi] function names: `play_sound` → `playSound`.
// The existing tauri-client renderer writes `invoke('play_sound', …)`. To let
// those callsites port unchanged, normalize snake_case method names here.
// Method names that arrive already in camelCase or single-word pass through
// untouched.
function normalizeMethod(method: string): string {
  return method.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());
}

export function invoke<T = unknown>(method: string, args?: unknown): Promise<T> {
  return window.decibell.invoke(normalizeMethod(method), args ?? {}) as Promise<T>;
}

export function listen<T = unknown>(
  name: string,
  handler: (event: Event<T>) => void,
): Promise<UnlistenFn> {
  return window.decibell.listen(name, handler as (e: Event<unknown>) => void);
}
