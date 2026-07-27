# Frontend review — chat scroll jank & general audit (2026-07-27)

> **Follow-up (2026-07-27, same day):** the B1 fix shipped a regression —
> `firstItemIndex` makes Virtuoso pass `itemContent` an *absolute* index
> (starting at firstItemIndex, verified: 1000000 for `data[0]`), so every
> `messages[index - 1]` lookup went out of bounds and message grouping
> stopped working entirely. Rebased in both panels. Added as **B8** below.
> Attachments were also still flashing after B3/B4 — the remaining cause
> was main-process, not renderer: **B9**.
>
> **Status (2026-07-27):** B1–B9 fixed, O1/O3/O4 done. **O2 investigated
> and deliberately closed** — measured payoff is ~26 ms of cold start and
> every way of collecting it costs more than it returns; see the item for
> numbers so it doesn't get re-opened. B6's fix is sound and builds clean
> but was **not verified against a real playing video** — worth a manual
> check while scrolling a channel with a video mid-playback.

Review of `electron-client/src` (renderer only). Started from a reported
symptom — *"small jumps when attachments scroll into view, and they
sometimes glitch in for a fraction of a second"* — then widened to a
general pass.

Every item below was verified against source; file:line references are
from `ui-rework` @ `92541b8`. Nothing here is speculative — where I
could not confirm a claim I dropped it rather than list it as a maybe.

Follows the numbering convention of `2026-06-11-client-code-review.md`:
`B` = bug, `O` = optimization. Priority order within each group.

---

## The reported symptom

Two separate causes, which is why it presents as two different
artifacts (a jump *and* a flash).

### B1 — History pagination has no scroll anchor `✓ FIXED`

`ChatPanel.tsx:494` and `DmChatPanel.tsx:389` both render `<Virtuoso>`
without `firstItemIndex`, while `startReached` prepends a page of older
messages (`chatStore.ts:441 prependHistory`, and the DM equivalent).

react-virtuoso keys its scroll anchoring off `firstItemIndex`; without
it, prepending N items shifts every index by N and the library has no
way to know the content above the viewport grew. The viewport stays at
the same *index*, which is now a different message — so the list jumps
by roughly the height of the page that just loaded. Attachment-heavy
histories jump furthest because their rows are tallest.

**Fix:** track a `firstItemIndex` that decrements by the number of
prepended messages, and pass it to both panels.

### B2 — No `computeItemKey`, so prepending remounts every visible row `✓ FIXED`

Neither panel passes `computeItemKey`, so Virtuoso falls back to index
keys. Combined with B1, a prepend changes the key of every mounted row,
and React unmounts and rebuilds the entire visible window instead of
moving it. `memo(MessageBubble)` (`MessageBubble.tsx`, bottom) buys
nothing across that boundary — it's a different element.

Also means images and video posters in view are re-created and re-fetched
on every page load.

**Fix:** `computeItemKey={(i, m) => m.id || m.nonce || i}`. `Message.id`
is 0 for optimistic bubbles and `Message.nonce` is the client UUID they
carry for exactly this purpose (`types/index.ts:225,236`), so the pair
covers both states; the index tail is only a guard against a real
message arriving with neither.

### B3 — `loading="lazy"` inside a virtualized list `✓ FIXED`

`AttachmentList.tsx:251`. Virtuoso only mounts rows that are already at
or near the viewport, so by the time the `<img>` exists it is about to
be visible. `loading="lazy"` then defers the *fetch* until it actually
intersects — the row scrolls in, the box is empty, and the image paints
a beat later. That is the "glitches in for a fraction of a second".

Lazy loading is doing no work here that virtualization isn't already
doing, and it costs a visible pop.

**Fix:** drop `loading="lazy"`, add `decoding="async"`. Pairs with B4.

### B4 — Rows mount exactly at the viewport edge `✓ FIXED`

No `increaseViewportBy` on either `<Virtuoso>`. Any post-mount settle —
image decode, poster frame arriving, an attachment resizing — happens
on-screen. With a viewport overscan the row mounts off-screen, settles,
and scrolls in already correct.

**Fix:** `increaseViewportBy={{ top: 600, bottom: 600 }}` (roughly two
tall rows).

### B5 — Every channel's first paint sizes attachments twice `✓ FIXED`

`chatStore.chatViewSize` starts `null` (`chatStore.ts:270`) and is only
seeded inside a `useEffect` (`ChatPanel.tsx:73-91`), which runs *after*
the first paint. So frame 1 sizes every attachment with the
unknown-viewport fallback caps (400×360, `attachmentSizing.ts:17-18`)
and frame 2 re-sizes them with the sqrt-derived caps. Every attachment
row changes height at once and Virtuoso re-measures.

Visible on channel open and on every channel switch — the `key=` on
`<Virtuoso>` forces a full remount, so this replays each time.

**Fix:** measure with `useLayoutEffect`, or derive the caps from a
container the parent already knows the width of, so the first render is
the correct one.

### B6 — The persistent video trails the list while scrolling `✓ FIXED (unverified)`

`PersistentVideoLayer.tsx:149-155` repositions its `position: fixed`
`<video>` from a scroll listener via `requestAnimationFrame`. The
element is therefore always at least one frame behind the placeholder it
is meant to be pinned to, so an actively-playing video visibly slides
against the rest of the list during a scroll.

Only affects the one video that is currently playing, which is why it
reads as occasional.

**Fix:** drive the overlay from the same scroll frame as the list
(Virtuoso's `scrollerRef` + a passive listener that writes a transform
rather than `top`/`left`), or accept it and hide the overlay for the
duration of a fling.

---

## Other bugs

### B7 — `listen()` unsubscribe race in `AppLayout` `✓ FIXED`

`AppLayout.tsx:41-53`:

```ts
let unlistenFn: (() => void) | null = null;
listen<UpdateEventPayload>("update_status", …).then((u) => { unlistenFn = u; });
return () => { if (unlistenFn) unlistenFn(); };
```

If the component unmounts before the promise resolves, `unlistenFn` is
still `null`, the cleanup no-ops, and the listener leaks. A remount then
attaches a second one and `update_status` is handled twice.

`useChatEvents.ts:100` and `useDmEvents.ts:107` already use the correct
form (`unlistenPromise.then((fn) => fn())`) — `AppLayout` is the only
site that got it wrong.

### B8 — `firstItemIndex` makes `itemContent`'s index absolute `✓ FIXED`

Introduced by the B1 fix, caught in review the same day.

With `firstItemIndex` set, react-virtuoso passes `itemContent` the
absolute index — it starts at `firstItemIndex`, not 0. Verified
directly: `data[0]` arrives as `index === 1000000`.

Both panels used that index to reach into the array
(`messages[index - 1]` for grouping, `index === messages.length - 1`
for `isLast`). Every lookup was ~1e6 out of bounds and returned
`undefined`, so `shouldGroup` hit its `if (!prev) return false` guard on
every row and **nothing grouped any more** — consecutive messages from
the same sender each rendered a full avatar row.

**Fixed:** rebase once at the top of `itemContent`
(`const i = index - firstItemIndex`) and index with that. Verified
against a seeded set: same sender +30s and +2m group, different sender
doesn't, same sender +12m doesn't, `isLast` true only on the last row.

**Lesson for next time:** `firstItemIndex` silently changes the meaning
of every other index-taking prop. Anything deriving state from
`itemContent`'s index has to be audited when it's introduced.

### B9 — Attachment responses aren't cacheable `✓ FIXED`

B3 and B4 fixed the *first* appearance of an image, but scrolling a row
out and back in still flashed.

The cause is in main, not the renderer:
`electron/main/protocol.ts` returned the upstream response verbatim, so
caching was entirely at the community server's discretion. With no
`Cache-Control` on that response Chromium caches nothing, and since
virtualization recreates the `<img>` on every remount, each scroll-back
was a fresh HTTPS round-trip with an empty box until it landed.

Attachment bytes are immutable — the id is server-assigned and unique,
and thumbnail variants carry their size in the query string — so the
handler now sets `Cache-Control: private, max-age=31536000, immutable`
when the server didn't specify one. Applied only to 200 responses; a
206 is a range slice and caching those under a single key would be
wrong.

Also gated the two per-request `console.log`s behind `!app.isPackaged`:
a fast scroll fires one pair per attachment and main-process stdout
writes are synchronous.

---

## Optimizations

### O1 — One global counter re-renders every mounted video `✓ FIXED`

`AttachmentList.tsx:323`: every `VideoItem` subscribes to
`useVideoCacheVersionStore((s) => s.version)`, a single global integer
bumped whenever *any* video captures a poster frame. Capturing one
poster re-renders every video attachment on screen.

**Fix:** key the subscription by attachment id, or move the poster into
the per-attachment cache entry the component already reads.

### O2 — Renderer ships as one 9.4 MB chunk `✗ NOT WORTH FIXING`

**Investigated 2026-07-27 and closed deliberately — do not re-open
without new numbers.**

`index-*.js` is 9,366 kB (1,811 kB gzip), and `src/components/emoji/
twemoji-data.json` is **8.02 MB of it** — emitted as raw JS string
literals, not `JSON.parse`, and statically imported by both
`Twemoji.tsx` and `RichInput.tsx`, so it is fully evaluated at boot.

That sounds severe. It isn't. Measured by `require()`ing an equivalent
module in **fresh processes** (the earlier `JSON.parse` and `vm.Script`
timings were both misleading — V8 defers work in the latter):

| | cold load |
| --- | --- |
| empty module | 0.2–0.3 ms |
| the emoji payload | **25.5 / 27.5 / 26.2 ms** |

So the entire prize is **~26 ms of cold start**, plus ~20 MB of heap.

There is no cheap way to collect it:

- **Shrinking the payload doesn't work.** It's already tight — zero
  whitespace to collapse, and hoisting the `<svg …>` opening tag that
  97% of entries share saves 0.21 MB of 7.77 MB. The rest is genuine
  path data.
- **`manualChunks` alone buys nothing.** A separate chunk that is still
  statically imported still parses at startup. It only helps caching,
  which is irrelevant behind `file://`.
- **Lazy-loading costs more than it saves.** The map is consumed
  synchronously during render, so deferring it means falling back to
  native glyphs until it lands — a visible emoji flash on cold start
  for anyone with auto-login, which is precisely the class of artifact
  B3 was raised to remove. Trading a guaranteed flash for 26 ms nobody
  perceives is a bad deal.

Also worth noting the 8 MB is a *deliberate* trade documented at the
top of `Twemoji.tsx`: it replaced `<img src=…>` emoji, which retained
50–80 MB of Chromium image cache. Undoing it to save 26 ms would cost
far more memory than it returns.

**Verdict:** leave it. Silence the Vite warning with
`build.chunkSizeWarningLimit` if the noise bothers anyone.

### O3 — Two store reads per attachment for one value `✓ FIXED`

`AttachmentList.tsx:182-183` and `277-278` each subscribe to
`chatViewSize.width` and `.height` separately, so every attachment holds
two subscriptions. With a 20-image grid that's 40 subscriptions
recomputing on each `ResizeObserver` fire, and the observer fires on
sub-pixel changes during a window drag.

**Fix:** one selector returning a stable memoized pair, and round the
observed size before storing it so sub-pixel noise doesn't publish.

### O4 — `useStreamThumbnails` re-subscribes on stream count changes `✓ FIXED`

`useStreamThumbnails.ts:51` deps on `[activeStreams.length]`, tearing
down and re-establishing the IPC subscription whenever a stream starts
or stops. The callback already reads fresh state via `getState()`, so
the dep buys nothing.

**Fix:** `[]`, with the existing early-return moved inside the callback.

---

## Checked and found healthy

Worth recording so these don't get re-reviewed:

- **Typecheck is clean.** `npm run typecheck` → 0 errors, both
  `tsconfig.web.json` and `tsconfig.node.json`. The baseline has been 0
  since the 2026-07-25 store-typing fix, so any new error is introduced
  by the change that surfaced it.
- **Blob URL lifecycle.** Every `createObjectURL` has a matching revoke
  path — `avatarStore`, `attachmentsStore`, `videoPlaybackState`,
  `voiceStore` (stream thumbnails revoke the previous URL before
  replacing), and both cropper modals.
- **Channel cache eviction.** `chatStore` prunes per-channel state
  beyond `uiStore.channelCacheSize`; RAM stays bounded on long sessions.
- **Store selectors.** The ones that compute inside the selector
  (`UserPanel.tsx:43`, `VoiceParticipantList.tsx:72`, `ChatPanel.tsx:105`,
  `ServerChannelsSidebar.tsx:380`) all return primitives or a stable
  sentinel (`EMPTY_CHANNELS`) — no new-object-per-render churn.
- **`dangerouslySetInnerHTML`.** Two uses, both fed from local constants
  (`FriendsPage` icon paths, `Twemoji` build-generated SVG map). No user
  content reaches either.
- **Index keys.** All three sites are static arrays (waveform bars, grid
  rows, loading skeletons), not identity-bearing lists.
- **Imperative wheel listener.** `AttachmentList.tsx:615` correctly uses
  `{ passive: false }` and removes on cleanup.

---

## Suggested order

B3 and B4 together are a one-line-each change that removes the flash and
most of the edge-mount settle — do those first and re-check whether the
remaining jump is still noticeable.

B1 + B2 are the real fix for the scroll-up jump and should land as one
change, since `firstItemIndex` without stable keys still remounts rows.

B5 is independent and only affects channel-open. B6 is cosmetic and only
while a video plays. B7 is a two-line correctness fix. O1–O4 are
non-urgent.
