import { useCallback, useMemo, useState, type JSX } from "react";
import data from "@emoji-mart/data";
import emojiRegex from "emoji-regex";
import Twemoji from "../../components/emoji/Twemoji";
import EmojiInfoPopover from "./EmojiInfoPopover";

// Lazily build a shortcode → native-unicode map from the emoji-mart dataset.
// Same dataset the Picker uses, so `:smile:` here always resolves to the
// same glyph the user would pick from the sheet.
let _shortcodeMap: Map<string, string> | null = null;
function getShortcodeMap(): Map<string, string> {
  if (_shortcodeMap) return _shortcodeMap;
  _shortcodeMap = new Map();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const emojis = (data as any).emojis as Record<
    string,
    { skins?: { native?: string }[] }
  >;
  for (const [id, emoji] of Object.entries(emojis)) {
    const native = emoji.skins?.[0]?.native;
    if (native) _shortcodeMap.set(id, native);
  }
  return _shortcodeMap;
}

let _nativeToIdMap: Map<string, string> | null = null;
function getNativeToIdMap(): Map<string, string> {
  if (_nativeToIdMap) return _nativeToIdMap;
  _nativeToIdMap = new Map();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const emojis = (data as any).emojis as Record<
    string,
    { skins?: { native?: string }[] }
  >;
  for (const [id, emoji] of Object.entries(emojis)) {
    const native = emoji.skins?.[0]?.native;
    if (native) _nativeToIdMap.set(native, id);
  }
  return _nativeToIdMap;
}

const SHORTCODE_RE = /:([a-z0-9_+-]+):/gi;

function expandShortcodes(text: string): string {
  const map = getShortcodeMap();
  return text.replace(SHORTCODE_RE, (match, id) => {
    const native = map.get(String(id).toLowerCase());
    return native ?? match;
  });
}

interface MessageTextProps {
  content: string;
  emojiSize?: number;
  /// Preview mode: shortcodes still expand and emojis still render
  /// through twemoji, but emojis are not click-targets (no popover,
  /// no jumbo sizing) and no popover element is rendered at all.
  /// Used by the DM sidebar's last-message preview rows where the
  /// outer button owns the click semantics.
  preview?: boolean;
}

const INLINE_EMOJI_SIZE = 24;

function jumboSizeFor(count: number): number {
  if (count === 1) return 56;
  if (count === 2) return 44;
  if (count === 3) return 38;
  return 32;
}

export default function MessageText({ content, emojiSize, preview }: MessageTextProps) {
  const [popover, setPopover] = useState<{
    emoji: string;
    anchor: HTMLElement;
  } | null>(null);

  const handleEmojiClick = useCallback(
    (e: React.MouseEvent<HTMLElement>, emoji: string) => {
      const anchor = e.currentTarget;
      setPopover((prev) =>
        prev && prev.anchor === anchor ? null : { emoji, anchor },
      );
    },
    [],
  );

  const tokens = useMemo<(string | JSX.Element)[]>(() => {
    const expanded = expandShortcodes(content);
    const matches = Array.from(expanded.matchAll(emojiRegex())).map((m) => ({
      index: m.index ?? 0,
      text: m[0],
    }));

    let remainder = expanded;
    for (let i = matches.length - 1; i >= 0; i--) {
      const m = matches[i];
      remainder =
        remainder.slice(0, m.index) + remainder.slice(m.index + m.text.length);
    }
    const emojiOnly = matches.length > 0 && remainder.trim().length === 0;

    // Preview mode keeps a constant inline size — the sidebar row's
    // line-height stays predictable and emoji-only DMs don't blow up
    // to 56px in a 14px-tall preview row.
    const size =
      emojiSize !== undefined
        ? emojiSize
        : preview
          ? INLINE_EMOJI_SIZE
          : emojiOnly
            ? jumboSizeFor(matches.length)
            : INLINE_EMOJI_SIZE;

    const out: (string | JSX.Element)[] = [];
    let last = 0;
    for (const m of matches) {
      if (m.index > last) out.push(expanded.slice(last, m.index));
      out.push(
        <Twemoji
          key={`${m.index}-${m.text}`}
          emoji={m.text}
          size={size}
          onClick={preview ? undefined : (e) => handleEmojiClick(e, m.text)}
        />,
      );
      last = m.index + m.text.length;
    }
    if (last < expanded.length) out.push(expanded.slice(last));
    return out;
  }, [content, emojiSize, handleEmojiClick, preview]);

  const shortcode = popover ? getNativeToIdMap().get(popover.emoji) ?? null : null;

  return (
    <>
      {tokens}
      {!preview && popover && (
        <EmojiInfoPopover
          anchor={popover.anchor}
          emoji={popover.emoji}
          shortcode={shortcode}
          source="Default"
          onClose={() => setPopover(null)}
        />
      )}
    </>
  );
}
