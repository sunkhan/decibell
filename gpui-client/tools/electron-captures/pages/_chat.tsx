// Shared by the chat pages (chat-fixture, chat-features): the renderer's real ChatPanel (and so
// RealMessageList, MessageBubble and everything under them) fed the GPUI chat fixtures
// (crates/decibell-chat/src/fixture.rs, exported by
// `cargo run -p decibell-chat --features fixtures --bin decibell-chat-fixture -- pages/fixtures`)
// through the renderer's own event hooks, like the shell pages (_shell.tsx).
//
// `setup()` returns every row's `offsetTop` / `offsetHeight` and its fractional box: the numbers
// the GPUI list's `chat-rows` self-test dumps on its side (tools/electron-captures/chat-rows.mjs
// compares the two).
import { MemoryRouter } from "react-router-dom";
import ChatPanel from "@app/features/chat/ChatPanel";
import { useUserDirectory } from "@app/features/users/userDirectory";
import { useAuthEvents } from "@app/features/auth/useAuthEvents";
import { useServerEvents } from "@app/features/servers/useServerEvents";
import { useChatEvents } from "@app/features/chat/useChatEvents";
import { useFriendsEvents } from "@app/features/friends/useFriendsEvents";
import { usePresenceEvents } from "@app/features/presence/usePresenceEvents";
import { replay, type ShellFixture } from "./_shell";
import { frames } from "../runtime/harness";

/// The hooks App.tsx mounts that the chat reads through (the shell page's set, less voice / search).
function Hooks() {
  useUserDirectory();
  useAuthEvents();
  useServerEvents();
  useChatEvents();
  useFriendsEvents();
  usePresenceEvents();
  return null;
}

export function ChatPage() {
  return (
    <MemoryRouter>
      <Hooks />
      <div className="flex h-full w-full">
        <ChatPanel />
      </div>
    </MemoryRouter>
  );
}

export async function replayChat(fixture: ShellFixture) {
  const shown = await replay(fixture);
  await frames(6);
  const scroller = document.querySelector<HTMLElement>(".overflow-y-auto");
  const rows = [...document.querySelectorAll<HTMLElement>("[data-key]")];
  const first = rows[0]?.getBoundingClientRect().top ?? 0;
  // What Chromium copies for a selection across messages: from the start of the first row
  // that opens a group (its name) to the end of the fourth row after it, and from inside one
  // body to inside the next. `Selection.toString()` is the text/plain the clipboard gets.
  const copied = (() => {
    const texts = rows.filter((row) => row.querySelector(".select-text")?.textContent);
    const start = texts.findIndex((row) => row.querySelector(".select-text .font-channel"));
    if (start < 0 || start + 4 >= texts.length) return null;
    const selection = window.getSelection();
    if (!selection) return null;
    const range = document.createRange();
    const first = texts[start].querySelector<HTMLElement>(".select-text")!;
    const last = texts[start + 4].querySelector<HTMLElement>(".select-text")!;
    range.setStart(first, 0);
    range.setEnd(last, last.childNodes.length);
    selection.removeAllRanges();
    selection.addRange(range);
    const whole = selection.toString();
    const ids = texts.slice(start, start + 5).map((row) => Number(row.dataset.key));
    selection.removeAllRanges();
    return { ids, whole };
  })();
  return {
    ...shown,
    copied,
    list: scroller
      ? { clientWidth: scroller.clientWidth, clientHeight: scroller.clientHeight, scrollHeight: scroller.scrollHeight, scrollTop: scroller.scrollTop }
      : null,
    rows: rows.map((row) => {
      const rect = row.getBoundingClientRect();
      return { id: Number(row.dataset.key), offsetTop: row.offsetTop, offsetHeight: row.offsetHeight, top: rect.top - first, height: rect.height };
    }),
  };
}
