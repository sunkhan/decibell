import { useChatStore } from "../../stores/chatStore";
import type { PendingInvite } from "../../types";

// One grammar for `decibell://invite/…` links, shared by the chat
// autolinker, the invite card, the deep-link receiver and the browse
// view's paste box. The link is the code alone — central resolves it
// to the community's host:port (INVITE_RESOLVE; communities register
// every invite there and re-register on reconnect), so a link never
// has to expose an IP address:
//   decibell://invite/<code>                    (what InviteModal copies)
// The endpoint-carrying shapes older links used still parse:
//   decibell://invite/<host>:<port>/<code>
//   decibell://invite/<host>/<port>/<code>
// Host may be a bracketed IPv6 literal. Codes are the server's 10
// base32 characters (native uppercases before redeeming).

const INVITE_RE =
  /^decibell:\/\/invite\/(?:(?:(\[[0-9a-f:.]+\]|[^\s/:]+):(\d{1,5})|([^\s/:]+)\/(\d{1,5}))\/)?([A-Za-z0-9]{4,64})\/?$/i;

export function parseInviteLink(url: string): PendingInvite | null {
  const m = INVITE_RE.exec(url);
  if (!m) return null;
  const code = m[5];
  const rawHost = m[1] ?? m[3];
  if (rawHost === undefined) return { code };
  const host = rawHost.replace(/^\[|\]$/g, "");
  const port = parseInt(m[2] ?? m[4] ?? "", 10);
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) return null;
  return { code, host, port };
}

export function isInviteLink(url: string): boolean {
  return /^decibell:\/\//i.test(url) && parseInviteLink(url) !== null;
}

/// The link the Invites modal hands out: code only.
export function buildInviteLink(code: string): string {
  return `decibell://invite/${code}`;
}

/// The plain link's click: the same confirm-then-join flow a deep link
/// gets (DeepLinkJoinModal). The invite card's Join button skips the
/// confirmation — the card *is* the preview.
export function openInviteLink(url: string): void {
  const parsed = parseInviteLink(url);
  if (parsed) useChatStore.getState().setPendingInvite(parsed);
}
