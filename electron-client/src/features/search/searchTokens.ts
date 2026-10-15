import type { ChannelInfo, ServerMember } from "../../types";
import { PERM, hasBits } from "../servers/permissions";
import { nameOf } from "../../stores/usersStore";
import {
  parseDay,
  withFilter,
  type FilterKind,
  type HasKind,
  type SearchFilter,
  type SearchQuery,
} from "./searchQuery";

/// Typed filters in the search box ("from:dana", "has:image",
/// "before:2026-10-01") and how they turn into chips.

export const OPS: FilterKind[] = ["from", "in", "has", "before", "during", "after"];

/// The filter being typed at the end of the text, if any.
export interface TypedToken {
  op: FilterKind;
  value: string;
  /// Where it starts in the text.
  start: number;
}

const TOKEN_RE = /(?:^|\s)(from|in|has|before|during|after):(\S*)$/i;

export function trailingToken(text: string): TypedToken | null {
  const m = TOKEN_RE.exec(text);
  if (!m) return null;
  const start = (m.index ?? 0) + (m[0].length - m[1].length - 1 - m[2].length);
  return { op: m[1].toLowerCase() as FilterKind, value: m[2], start };
}

/// What filters can name: members (loaded roster) and readable text channels.
export interface TokenContext {
  members: ServerMember[];
  channels: ChannelInfo[];
}

const HAS_ALIASES: Record<string, HasKind> = {
  link: "link", links: "link", url: "link",
  file: "file", files: "file", attachment: "file", attachments: "file",
  image: "image", images: "image", picture: "image", pictures: "image",
  video: "video", videos: "video",
  audio: "audio", sound: "audio", sounds: "audio",
  document: "document", documents: "document", doc: "document", docs: "document",
};

export function hasKindFor(value: string): HasKind | null {
  return HAS_ALIASES[value.toLowerCase()] ?? null;
}

/// A complete typed filter as a chip, or null if it names nothing known.
export function resolveToken(op: FilterKind, value: string, ctx: TokenContext): SearchFilter | null {
  const v = value.trim();
  if (!v) return null;
  switch (op) {
    case "from": {
      const lower = v.replace(/^@/, "").toLowerCase();
      const m =
        ctx.members.find((x) => nameOf(x.uid, x.username).toLowerCase() === lower) ??
        ctx.members.find((x) => x.nickname && x.nickname.toLowerCase() === lower);
      return m ? { kind: "from", uid: m.uid, username: nameOf(m.uid, m.username) } : null;
    }
    case "in": {
      const lower = v.replace(/^#/, "").toLowerCase();
      const c = ctx.channels.find((x) => x.name.toLowerCase() === lower);
      return c ? { kind: "in", channelId: c.id } : null;
    }
    case "has": {
      const k = hasKindFor(v);
      return k ? { kind: "has", has: k } : null;
    }
    case "inside":
      return null;   // an option, not a typed filter
    default: {
      const day = parseDay(v);
      return day ? { kind: op, date: day } : null;
    }
  }
}

/// Turns every complete, known filter in the text into a chip (used when a
/// search runs). Unknown ones stay as text.
export function chipTokens(q: SearchQuery, ctx: TokenContext): SearchQuery {
  let filters = q.filters;
  const kept: string[] = [];
  for (const word of q.text.split(/\s+/)) {
    if (!word) continue;
    const m = /^(from|in|has|before|during|after):(.+)$/i.exec(word);
    const f = m ? resolveToken(m[1].toLowerCase() as FilterKind, m[2], ctx) : null;
    if (f) filters = withFilter(filters, f);
    else kept.push(word);
  }
  return { text: kept.join(" "), filters };
}

/// Text channels a member can search (VIEW + READ_HISTORY there).
export function isSearchableChannel(c: ChannelInfo, isOwner: boolean): boolean {
  if (c.type !== "text") return false;
  if (isOwner) return true;
  return hasBits(c.myPermissions ?? 0, PERM.VIEW_CHANNEL | PERM.READ_HISTORY);
}

/// The category a channel sits under ("" when none).
export function categoryOf(channels: ChannelInfo[], id: string): string {
  let cat = "";
  for (const c of channels) {
    if (c.type === "category") cat = c.name;
    if (c.id === id) return cat;
  }
  return "";
}
