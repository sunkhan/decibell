// Shared by the composer pages (composer-*, emoji-picker*, gif-picker*, code-panel, math-panel,
// inline-editor): the renderer's real ChatPanel on the chat fixture (`_chat.tsx`), then the
// composer put in the state the GPUI screen of the same name shows (crates/decibell-composer/src/
// screens.rs). The texts and the fixture GIF page are the screens' own.
import fixture from "./fixtures/chat-fixture.json";
import { ChatPage } from "./_chat";
import { replay, type ShellFixture } from "./_shell";
import { useChatStore } from "@app/stores/chatStore";
import { useDraftsStore } from "@app/stores/draftsStore";
import { $, centre, click, frames, hover, respond, sleep } from "../runtime/harness";

export default ChatPage;

export const SERVER = "1";

/// The same draft as the GPUI screen `composer-typing`.
export const FORMATTED =
  "Release notes for **tonight**: `cargo build --release` then\n```rust\nfn main() {\n    println!(\"hello, decibell\");\n}\n```\nand the bound is $$\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}$$";

/// The chat fixture on screen, on `channel` (default `#general`).
export async function start(channel?: string) {
  const shown = await replay(fixture as unknown as ShellFixture);
  if (channel) {
    useChatStore.getState().setActiveChannel(channel);
  }
  await frames(8);
  return shown;
}

/// Puts `text` in the open channel's message box (what a refused send does: the editor shows it).
export async function typeDraft(channel: string, text: string) {
  useDraftsStore.getState().restoreChannelDraft(SERVER, channel, text);
  await frames(6);
}

/// A real click on the element `selector` finds.
export async function press(selector: string) {
  await click(...centre($(selector)));
  await frames(4);
}

/// Sets a controlled <input> / <textarea>'s value as typing would (React's onChange fires).
export async function fill(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  await frames(4);
}

/// The canned GIF page of the GPUI screens (`FixtureGifs::pages(16, 2, None)`): heights cycle.
export function gifPage(page: number, count: number, pages: number) {
  const heights = [120, 200, 160, 90, 240, 140];
  return {
    results: Array.from({ length: count }, (_, n) => {
      const height = heights[(n + page) % 6];
      return {
        id: `g${page}-${n}`,
        title: `Fixture GIF ${page}-${n}`,
        url: `https://static.klipy.com/fixture/${page}-${n}.gif`,
        width: 200,
        height,
        preview: "",
        previewWidth: 200,
        previewHeight: height,
      };
    }),
    next: page + 1 < pages ? String(page + 1) : null,
  };
}

export function gifs(configured: boolean) {
  respond("gifs.status", { configured, provider: configured ? "klipy" : null });
  respond("gifs.search", (_q: unknown, pos: unknown) =>
    configured ? { ok: true, page: gifPage(Number(pos ?? 0) || 0, 16, 2) } : { ok: false, error: "not-configured" },
  );
}

export { $, centre, click, frames, hover, sleep };
