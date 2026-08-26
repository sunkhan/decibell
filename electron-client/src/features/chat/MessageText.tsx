import { useCallback, useMemo, useState, type JSX } from "react";
import data from "@emoji-mart/data";
import emojiRegex from "emoji-regex";
import Twemoji from "../../components/emoji/Twemoji";
import EmojiInfoPopover from "./EmojiInfoPopover";
import { parseRichText, isPlainText, richTextToPlain, type RichNode } from "./richText";
import CodeBlock from "./CodeBlock";
import MathTex from "./MathTex";

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

  // Tokenize one plain-text run: shortcode expansion + Twemoji
  // replacement, exactly the pre-rich-text pipeline. Rich formatting
  // applies this per text leaf so emoji keep working inside emphasis
  // spans (but never inside code or math, which are verbatim).
  const emojiTokens = useCallback(
    (
      text: string,
      size: number,
      keyBase: string,
    ): { tokens: (string | JSX.Element)[]; matchCount: number; remainder: string } => {
      const expanded = expandShortcodes(text);
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
      const out: (string | JSX.Element)[] = [];
      let last = 0;
      for (const m of matches) {
        if (m.index > last) out.push(expanded.slice(last, m.index));
        out.push(
          <Twemoji
            key={`${keyBase}-${m.index}-${m.text}`}
            emoji={m.text}
            size={size}
            onClick={preview ? undefined : (e) => handleEmojiClick(e, m.text)}
          />,
        );
        last = m.index + m.text.length;
      }
      if (last < expanded.length) out.push(expanded.slice(last));
      return { tokens: out, matchCount: matches.length, remainder };
    },
    [handleEmojiClick, preview],
  );

  const tokens = useMemo<(string | JSX.Element)[]>(() => {
    // Preview mode (DM sidebar rows): no rich formatting — a code
    // block or display math inside a 14px single-line preview would
    // wreck the row — but no literal markers either: flatten the rich
    // tree to plain text (markers dropped, code/TeX contents kept), the
    // way Discord's conversation list previews read. Emoji keep a
    // constant inline size.
    if (preview) {
      const size = emojiSize !== undefined ? emojiSize : INLINE_EMOJI_SIZE;
      return emojiTokens(richTextToPlain(content), size, "p").tokens;
    }

    const nodes = parseRichText(content);

    // Unformatted message: the original path, jumbo sizing included.
    if (isPlainText(nodes) || nodes.length === 0) {
      const { tokens: out, matchCount, remainder } = emojiTokens(
        content,
        INLINE_EMOJI_SIZE,
        "t",
      );
      const emojiOnly = matchCount > 0 && remainder.trim().length === 0;
      const size =
        emojiSize !== undefined
          ? emojiSize
          : emojiOnly
            ? jumboSizeFor(matchCount)
            : INLINE_EMOJI_SIZE;
      if (size === INLINE_EMOJI_SIZE) return out;
      return emojiTokens(content, size, "t").tokens;
    }

    // Formatted message: map the rich-text nodes to elements, emoji-
    // tokenizing every text leaf. Expensive nodes (code blocks, math)
    // are capped per message so a pathological paste can't queue
    // dozens of KaTeX/highlight runs; overflow renders as inline code.
    const budget = { expensive: 10 };
    const size = emojiSize !== undefined ? emojiSize : INLINE_EMOJI_SIZE;
    const renderNodes = (
      list: RichNode[],
      keyBase: string,
    ): (string | JSX.Element)[] => {
      const out: (string | JSX.Element)[] = [];
      list.forEach((node, i) => {
        const key = `${keyBase}-${i}`;
        switch (node.kind) {
          case "text":
            out.push(...emojiTokens(node.text, size, key).tokens);
            break;
          case "bold":
            out.push(<strong key={key}>{renderNodes(node.children, key)}</strong>);
            break;
          case "italic":
            out.push(<em key={key}>{renderNodes(node.children, key)}</em>);
            break;
          case "underline":
            out.push(<u key={key}>{renderNodes(node.children, key)}</u>);
            break;
          case "strike":
            out.push(<s key={key}>{renderNodes(node.children, key)}</s>);
            break;
          case "code":
            out.push(
              <code
                key={key}
                className="box-decoration-clone rounded-sm bg-bg-darkest px-1.5 py-0.5 font-mono text-[0.85em]"
              >
                {node.text}
              </code>,
            );
            break;
          case "codeblock":
            if (budget.expensive-- > 0) {
              out.push(<CodeBlock key={key} lang={node.lang} text={node.text} />);
            } else {
              out.push(
                <code
                  key={key}
                  className="box-decoration-clone rounded-sm bg-bg-darkest px-1.5 py-0.5 font-mono text-[0.85em]"
                >
                  {node.text}
                </code>,
              );
            }
            break;
          case "math":
            if (budget.expensive-- > 0) {
              out.push(<MathTex key={key} tex={node.tex} display={node.display} />);
            } else {
              out.push(
                <code
                  key={key}
                  className="box-decoration-clone rounded-sm bg-bg-darkest px-1.5 py-0.5 font-mono text-[0.85em]"
                >
                  {node.tex}
                </code>,
              );
            }
            break;
        }
      });
      return out;
    };
    return renderNodes(nodes, "r");
  }, [content, emojiSize, emojiTokens, preview]);

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
