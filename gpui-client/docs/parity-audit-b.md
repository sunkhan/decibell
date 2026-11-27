# Parity audit B: communities, channels, text chat (FEATURES §4–§6)

2026-11-27, against the branch head `51be7d7` (`gpui-client`), with the frozen release build in
`~/.cache/decibell-dev/parity-root` and the local stack (central 127.0.0.1:18080, Daily Gaming / Rustaceans /
Design Lab). Every self-test was run alone, one process at a time, with a fresh `DECIBELL_USER_DATA_DIR`; a
failure was run again up to twice (other agents' windows share the desktop). The rows are in
`gpui-client/PARITY.md` §4–§6; this file has the reasons.

Paths: `R/` = `electron-client/src/`, `M/` = `electron-client/electron/main/`, `G/` = `gpui-client/crates/`.
Sizes: **S** under an hour, **M** hours, **L** a day or more.

## Summary

| Section | done | needs owner | partial |
|---|---|---|---|
| §4 Communities (12 rows) | 3 (4.1, 4.4, 4.12) | 0 | 9 |
| §5 Channels, roles and permissions (9 rows) | 4 (5.1, 5.5, 5.7, 5.9) | 0 | 5 |
| §6 Text chat (16 rows) | 2 (6.13, 6.14) | 0 | 14 |

No row is `needs owner`: every `partial` row has something a Linux machine with the stack can still close (a
missing piece, or a rule no test runs); the owner-only checks are listed per row in §3 and come after those.
Most `partial` rows are "built, but a rule is untested"; the real product gaps are §1.

## 1. Product gaps (missing or different from Electron), in priority order

1. **6.6 — Video and audio uploads carry no media probe** (M). Electron's `probeMetadata`
   (`R/features/chat/uploadAttachment.ts` ~230–345, 430–460) sends a video's width, height and duration and
   makes the thumbnails and ThumbHash from a poster frame (min(0.5 s, 10 %)); an audio file sends its duration.
   The native composer calls `decibell_attachments::upload::prepare(source, None)`
   (`G/decibell-model/src/chat/composer.rs:480`) and the `MediaProbe` trait (`G/decibell-attachments/src/thumbs.rs:255`)
   has no implementation anywhere (`wip/media2` adds none). A video sent from the native client has no poster,
   no blur placeholder and an unknown box on **every** client; an audio card has no duration. Closing it: a
   `MediaProbe` on `decibell-media`'s demuxer / decoder (first frame at Electron's time, dims, duration) and a
   self-test that attaches a clip and asserts the stored row's dims, duration and thumbnail mask.
2. **6.3 — No emoji info popover** (M). Electron: clicking an emoji in a message opens `EmojiInfoPopover`
   (the emoji at 36 px, its `:shortcode:`, "Default"; closes on an outside press, Esc or any scroll;
   `R/features/chat/MessageText.tsx:138, 285–297`, `R/features/chat/EmojiInfoPopover.tsx`). FEATURES names it. Native:
   nothing; a row has no emoji hit target (`G/decibell-chat/src/row.rs:69–91`, `Target`).
3. **6.16 — Message times have no full date-time tooltip** (S–M). Electron puts
   `title={formatFullDateTime(…)}` on the header's time and on a grouped row's hover time
   (`R/features/chat/MessageBubble.tsx:474, 535`). Native rows have no hit target for the stamp
   (`G/decibell-chat/src/row.rs:69–91`) and the list's tooltips are fixed strings (`G/decibell-chat/src/list/view.rs:72`,
   `title_of`). The formatter exists (`decibell_model::regional::Regional::full_date_time`). Both channel and DM
   lists.
4. **4.9 / 5.6 — Alt+↑ / Alt+↓ role reordering is missing** (S). Electron: `R/features/servers/tabs/RolesTab.tsx:340–348`
   moves the focused role one place. Native: no key handler in `G/decibell-server-settings/src/server/roles.rs`,
   while the row's tooltip says "Drag to reorder (Alt+↑/↓)" (`roles.rs:1009`). (HANDOFF §1 lists it as open.)
5. **6.2 — The code panel neither keeps lines unwrapped nor colours as typed** (M). Electron's code area is
   `wrap="off"` with a live `highlightNodes` overlay (`R/features/chat/RichComposer.tsx:60–75`). Native wraps and
   draws one colour (`G/decibell-composer/src/panel.rs:9–12`); the difference is written in the module doc
   only, not in `docs/electron-quirks.md`, so it stands as a gap until the owner decides.
6. **6.6 — Pasting a picture on native Wayland can paste text instead** (S to test, M to fix). GPUI's Wayland
   clipboard returns text whenever text is offered and only then looks for an image
   (`gpui-client/vendor/gpui-src/crates/gpui_linux/src/linux/wayland/clipboard.rs:239–241`); a browser's "Copy
   image" offers both. Electron's `usePasteToAttach` takes the file items. `composer-paste-image` runs under
   XWayland only, so the Wayland path is unchecked.
7. **6.1 — "Loading history…" and Retry follow other conditions** (S). Electron keeps "Loading history…"
   invisible for 300 ms and fades it in (`animate-[fadeUp_0.3s_ease_0.3s_both]`), shows it while
   `awaitingFirstPage`, and shows Retry when the first page failed even with live rows present
   (`R/features/chat/ChatPanel.tsx:1068–1076`). Native shows it at once, only while `loading && rows == 0`, and
   Retry only when `rows == 0` (`G/decibell-chat/src/pane.rs:323–331`): the text flashes on every fast load.
8. **6.10 — The delete confirmation's tip differs** (S). Electron: "Tip: hold [Shift key cap] while clicking the
   trash icon to skip this confirmation next time." (`R/components/DeleteMessageConfirmModal.tsx:33–40`). Native:
   plain "Tip: hold Shift when clicking delete to skip this confirmation." (`G/decibell-chat/src/pane.rs:360`,
   `G/decibell-home/src/dm/conversation.rs:454`, screen `G/decibell-chat/src/screens.rs:117`); the kit already has
   the exact one (`G/decibell-ui/src/kit.rs:519–541`).
9. **6.11 — The quoted reply line has no "Jump to message" tooltip** (S). `R/features/chat/ReplyPreview.tsx:174`;
   native `title_of` (`G/decibell-chat/src/list/view.rs:72–80`) has no case for it.
10. **6.15 — The disabled "Relevant" sort gives no reason** (S). Electron's title: "Not available when matching inside
    words" / "Add words to sort by relevance" (`R/features/search/SearchResultsPanel.tsx:136–138`). Native disables
    the segment (`G/decibell-search/src/results.rs:388–403`); the kit's `Segment` has no tooltip
    (`G/decibell-ui/src/toggles.rs:234`). Also trivial: a result's attachment thumbnail has the filename as its
    title in Electron (`SearchResultsPanel.tsx:431`), none here.

**Differences that are reasoned in code but not recorded in `docs/electron-quirks.md`** (each S: record it for the
owner, or align):

- 4.7 — the welcome banner's dashed ring stands still (renderer: turns forever, `welcome-spin-slow` 20 s); reasoned
  in `G/decibell-servers/src/welcome.rs:12–14` (no frames at rest), like quirk #1's glow.
- 5.6 — Create role is dimmed at 250 roles (`G/decibell-server-settings/src/server/roles.rs:748`); Electron has no
  such state (the server refuses).
- 6.6 — a refused upload says "the file is larger than this server allows" / "the server is out of space"
  (`G/decibell-attachments/src/error.rs:15–18`); Electron shows the raw "init failed: HTTP 413 …"
  (`R/features/chat/uploadAttachment.ts:569–571, 737`).
- 6.8 — the viewer's rotation is instant (`G/decibell-chat-content/src/viewer/mod.rs:19–21`); Electron animates a
  quarter turn over 260 ms (`R/features/chat/imageViewer/ImageViewer.tsx:353`).
- 6.12 — the pacing token is spent when the send call returns and a community's sends run one after another
  (`G/decibell-model/src/chat/pacing.rs:10–21`); Electron spends it on admission (`R/features/chat/sendPacing.ts:57–59`).
  Same for the user; record it.

**In flight, not counted as findings** (6.7, `wip/media2`, unmerged; `docs/media-design.md` §8): the controls'
fade and slide, hover thumbs, the fullscreen download button, Space on an audio card, the audio card's "Failed
to load" with retry, the last-frame poster, a DM conversation change stopping playback, rotation, 10-bit, VP9 on
the GPU, attachment sound on the device Settings → Audio names (on the head it still plays on the system default,
contradicting the CLAUDE.md rule, and quirk #19 still says "Mirrors"), the retained-memory fix, the Electron
comparison, and the self-tests `media-keys`, `media-poster`, `media-output-device`, `media-sealed`,
`media-rotation`, `media-vp9-hw`, `media-10bit`. Also in flight: the type-to-focus fixes in `wip/integrate-d`
(`0ad76c48`, `2f156e9f`) — see §4, `composer-type-to-focus` and `join-dm-composer` fail on the head.

## 2. Missing tests (built, but no test runs the rule)

Each can be a self-test against the local stack unless marked otherwise.

**§4**
- 4.2 — the manual "host & port" join of an unlisted community (Rustaceans) (S); the join dialog's invalid-code
  state ("This invite is unknown, expired, or used up.") and Cancel (S); Browse's search filtering cards (S).
- 4.3 — a second session of a throwaway account joins a community and the first shows and connects the tile
  (`MEMBERSHIP_ADDED`), as `settings-devices-signout` runs two sessions (M); a refused auto-rejoin (banned while
  signed out → "Membership revoked", tile dropped) (M). Both are unit-tested through the real router
  (`G/decibell-model/src/events/tests.rs` `refused_joins_revocations_and_leaving`, `lists_pictures_invites_and_bans`).
- 4.5 — upload and remove a server picture from Overview on a stack community, and a second session seeing the
  new version (M).
- 4.6 — Copy link was **skipped** in today's run (`invite-manage-live` leaves a non-empty clipboard alone: "SKIP
  the clipboard check"); Copy code is never pressed; the invite card's Join, Open and Invalid states
  (`G/decibell-chat-content/src/embeds.rs:359, 744, 791`) (S each).
- 4.7 — Manage channel's click opening channel settings; a member without CREATE_INVITE / MANAGE_CHANNELS seeing
  neither action (S).
- 4.8 — the owner's leave: Server Settings hides Leave for the owner; the server menu's Leave is refused by the
  server with a toast and nothing changes (S). Separately, the core can undo a leave by reconnecting before it
  acts on the answer (`G/decibell-servers/src/selftests.rs:847–863` logs it as a NOTE; shared with Electron,
  `decibell-core` `net/community.rs`); it did not happen in today's runs.
- 4.9 — role reorder by drag (M); a role's holders added and removed (S); the RGB picker driven as UI (S); the
  name and public-listing saves (S); Storage's minimum free space save (S); the Server Settings window's leave
  guard on Esc / ✕ / backdrop / tab switch (`channel-settings-echo` covers only Channel Settings' close) (S).
- 4.10 — transfer ownership is run by nothing (M; Design Lab: carol → a throwaway → back, or at least the picker
  and the typed confirmation without the final request).
- 4.11 — kick, timeout and End timeout, and a ban with an expiry and a message purge, from Members / the user
  menu on a throwaway in Design Lab (M). Voice moderation is 7.8's.
- 4.12 — later roster pages never load live: the stack's largest roster (299) fits the first page (500). Paging
  is unit-tested (`G/decibell-model/src/stores/community.rs` tests); a live run needs a community over 500
  members (a seed change, not this audit's to make).

**§5**
- 5.2 — a voice channel's bitrate (slider, Reset to the default) saved and echoed (S).
- 5.3 — the header drawing a saved topic live; the encrypted channel's "Topics aren't encrypted. The server host
  can read them." (S).
- 5.4 — a message posted and read in an encrypted channel (#vault), a second member decrypting it (escrowed
  keys), an edit staying sealed, and a sealed attachment sent and opened by another account (M; the crate-level
  `G/decibell-attachments/tests/stack.rs` covers the upload only; playback's `media-sealed` is in flight).
- 5.6 — drag reorder; role holders (as 4.9).
- 5.8 — a per-member overwrite through the target picker, and the tri-state toggles as clicks (the test stages
  bits with `stage_bits`) (S).

**§6** (the code audit of these rows was split with a helper; references checked)
- 6.1 — the Retry path ("Couldn't load messages." + Retry) and the toasts "Couldn't find that message" /
  "Couldn't load that message" (`G/decibell-model/src/actions/chat.rs:345–347`), possible with a fixture source
  whose load fails (S); `chat-switch` restores through the fixture harness's remount, not the real `ChatPane`
  following the navigation on the stack (S).
- 6.2 — the code and math panels (open from the menu, Tab / Shift+Tab, auto-indent, Ctrl+Enter, Escape) (S); a code
  block's copy button (clipboard + tick) and the Expand / Collapse bar (`G/decibell-chat/src/list/view.rs:1553–1575`) (S).
- 6.3 — "Frequently used" order and its persistence (S).
- 6.4 — Privacy's "Unfiltered GIF search" reaching the request as `content_filter=off`: `FixtureGifs` records it
  (`G/decibell-composer/src/gifs.rs:148`) but `gif-picker` never checks it (S). A real KLIPY search is not run
  (`cargo test -p decibell-unfurl --test live -- --ignored` would, at one quota request: the owner's call).
- 6.5 — invite cards shown with link previews off (`embeds.rs:169–173`) (S); a sender without EMBED_LINKS
  (`suppress_embeds`) (S); the real-internet unfurl test is ignored (runnable here).
- 6.6 — sealed attachments end to end in the app (#vault: send, envelope metadata, another account's card and
  thumbnail decrypted) (M); a 413 / 507 shown on the in-flight row (`G/decibell-chat-content/src/inflight.rs:128`) (S);
  the Wayland image paste (item 6 above).
- 6.7 (on the head, beyond media2's list) — the hover download button beside a card clicked
  (`content-download` calls `downloads::start` directly) (S); fullscreen by double-click, F and Esc (S); volume /
  mute and their persistence, and "Paused at m:ss" resuming (S).
- 6.8 — Save (Ctrl+S / button) writing the file; rotate (R); minimap presses; wheel zoom toward the pointer;
  Home / End / + / − / 0; Open in browser; the gallery being exactly the clicked message's pictures
  (`content-viewer` asserts only ≥ 2); the fly-in / out ending in idle (S each); no viewer screen exists, so the
  themed stage was never compared with Electron in the seven themes (M).
- 6.9 — a live encrypted download from #vault (M); the Uploads tab on the stack: pause / resume from its row and
  "Go to message" landing (`R/features/transfers/UploadRow.tsx:63–70`, `G/decibell-model/src/actions/transfers.rs:130`)
  (M); the risky-file confirm, Cancel, Retry, Clear, the "Deleted" row and the row menu as UI (one fixture test,
  S); the download speed cap observed live (S).
- 6.10 — the UI delete path (trash → confirm; Shift+click without the dialog): the tests call the action
  (`G/decibell-composer/src/selftests.rs:302`) (S); deleting another member's message with Manage Messages and the
  trash hidden without it (S); a refused delete restoring the row with its reason (S); an edit in #vault staying
  sealed with its attachments (M); "(edited)" as drawn (the test reads the stored edit time) (S).
- 6.11 — the quoted line jumping (also to a parent not loaded), the name opening the profile, the "Original
  message was deleted" line not clickable, the reply-to-you rail in channels and none in DMs, the rail on the
  message being answered, Escape cancelling (S each); `composer-reply` asserts only the target and the sent row's
  reply id (`G/decibell-composer/src/selftests.rs:583–601`) although its description names the rail.
- 6.12 — the 30 s echo watchdog (ask the server, then withdraw with "The server didn't confirm it…" and the text
  back; `G/decibell-model/src/actions/chat.rs:673`), with the clock advanced (M); "exactly that bubble" with two
  sends pending in one channel (S, unit).
- 6.13 (optional) — the owner not held in #slow-lane; a window started by another session corrected by the
  server's `retry_after_seconds` (S–M).
- 6.15 — has: file / video / audio / document / link (only has:image runs); Oldest / Relevant through the UI
  (`search-jump` sets the store); the capped "N+" count; the error state with "Try again"; the date picker; Escape
  closing the panel and Backspace removing a chip (S each).
- 6.16 — Settings → Appearance → Time format re-labelling the open list and outliving a restart
  (`settings-appearance-apply` covers theme, text size and density only) (S).

## 3. Owner-only checks

- 6.7 — the two-person listening test on attachment playback (no pop on play, pause, seek, end; lip sync); picture
  quality against Electron by eye, a phone portrait clip after media2; AMD / Intel decode.
- 6.6 — pick files in the real portal dialog, drop from Dolphin, paste a Spectacle screenshot and a browser's "Copy
  image" on Wayland: a chip each time.
- 6.9 — "Show in folder" opens the file manager with the file selected, Open launches the default app (the test
  checks the path only).
- 6.11 — the reply spine's look beside Electron (captures exist for rows: `chat-rows` heights; the spine by eye).
- Windows, for every row: see §5.

## 4. Self-tests run today

RESULTS_TABLE

## 5. Rows with no step in `docs/windows-owner-checks.md`

`windows-owner-checks.md` §2 explains how to reach the stack from Windows but names no test to run there, so every
stack-backed row of §4–§6 has no Windows step: 4.3, 4.5, 4.7, 4.8, 4.9, 4.10, 4.11, 4.12, 5.1–5.9, 6.2 (code / math
rendering), 6.4, 6.5, 6.7, 6.10, 6.11, 6.12, 6.13, 6.14, 6.15. One step would cover most of them: "from WSL2's stack,
run the self-tests of PARITY §4–§6 (list) with a `dev-local` build and send the summary". Rows covered only in
part: 4.1 (§2 setup), 4.2 / 4.6 (§7 `decibell://`), 4.4 (§8 fonts), 6.1 (§1 fixture chat tests), 6.3 (§8 emoji
drawing; the picker has none), 6.6 (§8 links, files, clipboard), 6.8 (§7 Copy image only), 6.9 (§7 taskbar
progress and quit guard, §8 Open / Show in folder), 6.16 (§8 regional formats).

On the runner (`ci/win-gpui-check-log`, run of `86c1b34`, no servers): `chat-resize`, `chat-selection`, `chat-rows`,
`chat-idle`, `chat-hover` PASS; `chat-history`, `chat-switch`, `input-typing`, `input-ime`, `input-atoms`,
`render-cost` FAIL (the Windows batch's round 3, `wip/windows`); unit tests of `decibell-richtext`, `-highlight`,
`-tex`, `-attachments` (incl. `tests/transfers.rs`, 12) and `-unfurl` pass. `decibell-model`, `-chat`,
`-chat-content`, `-composer`, `-media`, `-servers`, `-server-settings` unit tests do not run there.

## 6. Outside these rows

- `docs/FEATURES.md` §5 "Retention" says a purged attachment leaves a "cleaned up after N days" tombstone; Electron
  (and the native client) draw "<name> (deleted by retention policy)" (`R/features/chat/AttachmentList.tsx:935`).
  The catalogue's wording is wrong, not the clients.
- `docs/FEATURES.md` §6 "Media playback" says players survive channel switches; the code stops them (quirk #17,
  an open owner decision).
- `windows-owner-checks.md` §8 "Links, files" says Open on a downloaded program runs it, "a decision for the owner
  whether to ask first"; the native client already asks first, as Electron does.
- The PARITY.md header still lists the old status values (`not started · in progress · done · needs owner`);
  `partial` is used by this audit as the brief asks.
