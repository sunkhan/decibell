// Shared by the content pages (content-attachments, content-embeds, their tall variants, the image
// viewer's): the renderer's real ChatPanel (as `_chat.tsx` draws it) plus the app-level
// ImageViewer / ImageContextMenu, fed the GPUI content fixtures
// (crates/decibell-chat-content/src/fixture.rs, exported with the pictures by
// `cargo run -p decibell-chat-content --features fixtures --bin decibell-content-fixture --
// tools/electron-captures/pages/fixtures tools/electron-captures/.work/content-files`).
//
// The pictures come from shoot.cjs's `decibell-attachment:` protocol and https stub (the fixture's
// files: the "Still loading" one never answers, the "gone" one is a 404); the link previews and the
// invite answers are seeded into their stores before the history arrives, as when the channel was
// visited before (the GPUI fixture answers them from memory the same way).
import ImageViewer from "@app/features/chat/ImageViewer";
import ImageContextMenu from "@app/components/ImageContextMenu";
import { useLinkPreviewStore } from "@app/stores/linkPreviewStore";
import { useInviteResolveStore } from "@app/stores/inviteResolveStore";
import { useImageViewerStore } from "@app/stores/imageViewerStore";
import { useChatStore } from "@app/stores/chatStore";
import { galleryFor } from "@app/features/chat/imageViewer/gallery";
import type { LinkPreview, Message, ResolvedInvite } from "@app/types";
import { ChatPage } from "./_chat";
import { Framed } from "./_frame";
import { replay, type ShellFixture } from "./_shell";
import { frames, sleep } from "../runtime/harness";

export type ContentFixture = ShellFixture & {
  previews: Record<string, LinkPreview | null>;
  invites: Record<string, { resolved?: ResolvedInvite; error?: string }>;
};

export function ContentPage() {
  return (
    <>
      <ChatPage />
      <ImageViewer />
      <ImageContextMenu />
    </>
  );
}

/// The whole window (a 32 px title-bar band over the chat): the viewer places itself under it.
export function FramedContentPage() {
  return (
    <>
      <Framed>
        <ChatPage />
      </Framed>
      <ImageViewer />
      <ImageContextMenu />
    </>
  );
}

function seed(fixture: ContentFixture) {
  const at = Date.now();
  useLinkPreviewStore.setState({
    entries: Object.fromEntries(Object.entries(fixture.previews).map(([url, preview]) => [url, { status: "done" as const, preview }])),
  });
  useInviteResolveStore.setState({
    entries: Object.fromEntries(
      Object.entries(fixture.invites).map(([code, answer]) => [
        code,
        answer.resolved
          ? { status: "done" as const, resolved: answer.resolved, error: null, invalid: false, at }
          : {
              status: "done" as const,
              resolved: null,
              error: answer.error ?? "",
              invalid: /unknown|expired|not found|invalid/i.test(answer.error ?? ""),
              at,
            },
      ]),
    ),
  });
}

/// Waits (at most `ms`) for every picture in the page to be decoded: `<img>`s, and the
/// `background-image`s of the blur / video posters (decoded through an Image of the same URL).
export async function pictures(ms = 2500) {
  const urls = new Set<string>();
  const imgs = [...document.querySelectorAll<HTMLImageElement>("img")];
  for (const el of document.querySelectorAll<HTMLElement>("[style*='background-image']")) {
    const m = /url\("?([^")]+)"?\)/.exec(el.style.backgroundImage);
    if (m) urls.add(m[1]);
  }
  const decodes = [
    ...imgs.map((img) => (img.complete && img.naturalWidth > 0 ? Promise.resolve() : img.decode().catch(() => {}))),
    ...[...urls].map((url) => {
      const img = new Image();
      img.src = url;
      return img.decode().catch(() => {});
    }),
  ];
  await Promise.race([Promise.all(decodes), sleep(ms)]);
  await frames(4);
}

/// Every row's geometry, as `_chat.tsx` reports it (`chat-rows.mjs` reads it).
export function rows() {
  const scroller = document.querySelector<HTMLElement>(".overflow-y-auto");
  const list = [...document.querySelectorAll<HTMLElement>("[data-key]")];
  const first = list[0]?.getBoundingClientRect().top ?? 0;
  return {
    list: scroller
      ? { clientWidth: scroller.clientWidth, clientHeight: scroller.clientHeight, scrollHeight: scroller.scrollHeight, scrollTop: scroller.scrollTop }
      : null,
    rows: list.map((row) => {
      const rect = row.getBoundingClientRect();
      return { id: Number(row.dataset.key), offsetTop: row.offsetTop, offsetHeight: row.offsetHeight, top: rect.top - first, height: rect.height };
    }),
  };
}

export async function replayContent(fixture: ContentFixture) {
  seed(fixture);
  const shown = await replay(fixture);
  await frames(6);
  await pictures();
  // Which pictures are in (the "Still loading" one must still be pending, the gone one broken).
  const images = [...document.querySelectorAll<HTMLImageElement>("img")].map((img) => ({
    alt: img.alt,
    src: img.src.slice(0, 80),
    complete: img.complete,
    naturalWidth: img.naturalWidth,
  }));
  return { ...shown, ...rows(), images };
}

/// Opens the viewer on the "Two side by side" message's first picture (no origin rect: no
/// flight), as the GPUI `image-viewer` screen does; `zoom` presses `1` (actual size).
export async function openViewer(fixture: ContentFixture, zoom: boolean) {
  const shown = await replayContent(fixture);
  const all = Object.values(useChatStore.getState().messagesByChannel).flat() as Message[];
  const message = all.find((m) => m.attachments?.length === 2);
  if (!message) throw new Error("no two-picture message in the fixture");
  const server = useChatStore.getState().activeServerId ?? "1";
  const { items, index } = galleryFor(server, message, message.attachments, Number(message.attachments[0].id));
  useImageViewerStore.getState().open(items, index, null);
  await frames(6);
  await pictures(3000);
  if (zoom) {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "1", bubbles: true }));
    await sleep(450);
    await frames(4);
  }
  // The chrome hides after 2.5 s without a pointer move: a move keeps it up for the shot.
  document.querySelector(".image-viewer")?.dispatchEvent(new PointerEvent("pointermove", { bubbles: true }));
  await frames(2);
  return { ...shown, framed: true };
}
