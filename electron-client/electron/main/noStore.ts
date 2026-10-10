/// Spread into a `net.fetch` init to keep the response out of Chromium's
/// disk cache (Fetch `cache: "no-store"`). Electron honours the mode — a
/// no-store response is never stored (checked against a cacheable local
/// server) — but the Node `RequestInit` typing used here predates the field.
export const NO_STORE = { cache: "no-store" } as unknown as RequestInit;
