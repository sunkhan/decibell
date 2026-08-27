import { useChatStore } from "../../stores/chatStore";
import type { PendingInvite } from "../../types";

// One grammar for `decibell://invite/…` links, shared by the chat
// autolinker, the invite card, and the deep-link receiver. Accepted:
//   decibell://invite/<host>:<port>/<code>      (what InviteModal copies)
//   decibell://invite/<host>/<port>/<code>
// Host may be a bracketed IPv6 literal. Codes are the server's
// alphanumerics (native uppercases them before redeeming).

const INVITE_RE =
  /^decibell:\/\/invite\/(?:(\[[0-9a-f:.]+\]|[^\s/:]+):(\d{1,5})|([^\s/:]+)\/(\d{1,5}))\/([A-Za-z0-9-]{1,64})\/?$/i;

export function parseInviteLink(url: string): PendingInvite | null {
  const m = INVITE_RE.exec(url);
  if (!m) return null;
  const host = (m[1] ?? m[3] ?? "").replace(/^\[|\]$/g, "");
  const port = parseInt(m[2] ?? m[4] ?? "", 10);
  const code = m[5];
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) return null;
  return { host, port, code };
}

export function isInviteLink(url: string): boolean {
  return /^decibell:\/\//i.test(url) && parseInviteLink(url) !== null;
}

/// The plain link's click: the same confirm-then-join flow a deep link
/// gets (DeepLinkJoinModal). The invite card's Join button skips the
/// confirmation — the card *is* the preview.
export function openInviteLink(url: string): void {
  const parsed = parseInviteLink(url);
  if (parsed) useChatStore.getState().setPendingInvite(parsed);
}
