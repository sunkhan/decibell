// A stand-in for the preload bridge (`window.decibell`, electron-client/electron/preload/index.ts).
// Installed by main.tsx's first import, before any app module runs. Nothing talks to a server or
// to native code:
//   - every method call resolves to null (or to a canned answer from `respond`);
//   - `subscribe` and `on*` return an unsubscribe function `() => {}`;
//   - `listen` returns an unsubscribe function that is also awaitable (the real one is async), so
//     both `const un = listen(...)` and `listen(...).then(un => ...)` work;
//   - every call is recorded in `window.__calls` as { method, args }: `invoke("play_sound", a)` is
//     recorded as method "playSound" (camel-cased, as lib/ipc does), nested calls by path
//     ("dialog.open").
// Plain fields the app reads at startup get harmless values (platform, sentryConfig, ...).

export type StubCall = { method: string; args: unknown[] };
type Responder = unknown | ((...args: unknown[]) => unknown);

const calls: StubCall[] = [];
const responses = new Map<string, Responder>();

const camel = (s: string) => s.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());

function unsubscriber(): () => void {
  return () => {};
}

/** An unsubscribe function that can also be awaited / .then()ed to get one. */
function awaitableUnsubscriber() {
  const plain = unsubscriber();
  const f = unsubscriber() as (() => void) & {
    then: (ok?: (v: () => void) => unknown) => unknown;
    catch: () => unknown;
    finally: (cb?: () => void) => unknown;
  };
  f.then = (ok) => Promise.resolve(plain).then(ok);
  f.catch = () => Promise.resolve(plain);
  f.finally = (cb) => Promise.resolve(plain).finally(cb);
  return f;
}

function answer(method: string, args: unknown[]): unknown {
  if (!responses.has(method)) return null;
  const r = responses.get(method);
  return typeof r === "function" ? (r as (...a: unknown[]) => unknown)(...args) : r;
}

const FIELDS: Record<string, unknown> = {
  platform: "linux",
  mediaServerPort: 0,
  mediaServerToken: "",
  sentryConfig: { enabled: false, installId: "", version: "harness" },
};

function node(path: string[]): unknown {
  const target = function () {} as unknown as object;
  return new Proxy(target, {
    get(_t, prop) {
      // Not a thenable, not a primitive: `await window.decibell.x` must not hang.
      if (typeof prop === "symbol" || prop === "then" || prop === "toJSON") return undefined;
      const key = [...path, prop].join(".");
      if (key in FIELDS) return FIELDS[key];
      return node([...path, prop]);
    },
    apply(_t, _this, args: unknown[]) {
      const name = path[path.length - 1] ?? "";
      const full = path.join(".");
      if (full === "invoke") {
        const method = camel(String(args[0] ?? ""));
        calls.push({ method, args: [args[1]] });
        return Promise.resolve(answer(method, [args[1]]));
      }
      calls.push({ method: full, args });
      if (name === "listen") return awaitableUnsubscriber();
      if (name === "subscribe" || /^on[A-Z]/.test(name)) {
        const r = answer(full, args);
        return r ?? unsubscriber();
      }
      return Promise.resolve(answer(full, args));
    },
  });
}

/** Canned answer for an invoke method (camel-cased) or a bridge path ("gifs.status"). */
export function respond(method: string, value: Responder): void {
  responses.set(method, value);
}

declare global {
  interface Window {
    __calls: StubCall[];
    __stub: { respond: typeof respond; calls: StubCall[] };
  }
}

(window as unknown as { decibell: unknown }).decibell = node([]);
window.__calls = calls;
window.__stub = { respond, calls };
