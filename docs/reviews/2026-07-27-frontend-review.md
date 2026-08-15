# Frontend review — chat scroll jank & general audit (2026-07-27)

> ## Read this first if you're picking up the scroll-glitch work
>
> **Shipped in 0.6.10. B1–B11 fixed, O1/O3/O4 done, O2 closed
> deliberately. The user reports the glitch is still present** — that is
> the open thread.
>
> **2026-08-15 round: see the section of that date at the bottom.** It
> closes suspect #2 by argument, lands the decode-side fixes B12–B14
> (user-verified: drastic reduction), ships the suspect-#1
> instrumentation this header asks for — which then **caught suspect #1
> live** (B15, the page-boundary group-flip) — and fixes it with eager
> pagination.
>
> ### What is fixed and *verified*, so don't re-investigate it
>
> | | evidence |
> | --- | --- |
> | history pagination no longer jumps | `firstItemIndex` + stable keys; hook driven through a scripted prepend sequence |
> | images don't defer their fetch | `loading="lazy"` removed |
> | rows settle off-screen | `increaseViewportBy` 600px both panels |
> | attachments size once, not twice | viewport seeded in `useLayoutEffect` |
> | previews are warmed ahead of the viewport | `attachmentPrefetch.ts`, ±15 messages off `rangeChanged` |
> | a warmed image paints in its mount frame | measured: cold `complete=false`, warmed `complete=true` |
> | image rows hold their box before a URL exists | they used to render `null` → zero height → pop |
> | ThumbHash placeholders reach the renderer | confirmed live: 28-char hash on new uploads |
>
> ### Ruled out with measurements — don't repeat these
>
> - **HTTP caching was never the cause.** Warm 8 URLs, mount `<img>` for
>   them: **0** extra protocol hits with *or* without `Cache-Control`.
>   Chromium's in-page memory cache already answers a remount. The header
>   added in B9 is still correct for disk cache across restarts, but it
>   fixed nothing here.
> - **Bundle size is not a startup problem.** The 8 MB emoji payload
>   costs ~26 ms measured in fresh processes. See O2 before reopening.
>
> ### Where to look next
>
> Everything above targets *the image being absent*. If the glitch
> survives on freshly-uploaded attachments that carry a placeholder, the
> symptom is probably **not** the empty box — suspects worth measuring:
>
> 1. **Row height changing after mount.** Instrument
>    `MessageBubble`/`AttachmentList` with a `ResizeObserver` and log any
>    height delta after first paint. Virtuoso re-measures and corrects
>    scroll on every one.
> 2. **`reserveBox` when `attachment.width/height` are 0.** Falls back to
>    260×180 (`attachmentSizing.ts`) — if real dimensions arrive later,
>    every such row resizes.
> 3. **B6 was never verified against real playback** — the persistent
>    video overlay. If the glitch involves video specifically, start there.
> 4. **Capture a real trace.** No frame timings have ever been recorded
>    for this. A scripted scroll + Chromium tracing would say whether
>    frames are being dropped at all, which nothing so far establishes.
>
> ### Two traps this work already fell into
>
> - **`firstItemIndex` silently changes what other props mean.**
>   `itemContent` and `rangeChanged` become *absolute* (they start at
>   `firstItemIndex`); `initialTopMostItemIndex` does **not**. All three
>   verified by experiment. Getting this wrong broke message grouping and
>   scroll restore, silently, in ways that typecheck fine. See B8.
> - **Silent catch blocks cost three debugging rounds.** ThumbHash came
>   out empty for every pasted image because `fetch(blob:…)` is blocked by
>   `connect-src` and the encoder swallowed the rejection. It logs in dev
>   now. Prefer a dev-visible failure to a cosmetic fallback that hides
>   the cause.

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

`rangeChanged` has the same basis — verified, it reports
`startIndex: 1000150` for `data[150]`. Both panels stored that raw in
`topIndexRef` as the saved scroll position, and the restore path clamps
with `if (saved.topIndex > last) return last`, so a ~1e6 value was
silently rejected and **every channel switch landed at the bottom**
instead of where you left off. Rebased alongside the grouping fix.

`initialTopMostItemIndex` is *not* absolute — it stays an index into
`data`. Verified: passing 150 lands on `data[150]`.

**Lesson for next time:** `firstItemIndex` silently changes the meaning
of some index-carrying props (`itemContent`, `rangeChanged`) but not
others (`initialTopMostItemIndex`). Every one has to be checked
individually when it's introduced — the types don't distinguish them.

### B9 — Attachment responses aren't cacheable `✓ FIXED (but not the cause)`

> **Correction (same day):** the reasoning below was wrong about *why*
> this mattered, and the fix is smaller than claimed. Measured in
> Electron: warm 8 URLs with `new Image()`, then mount `<img>` for the
> same URLs, and count protocol-handler hits.
>
> | | hits after warm | after mounting | extra |
> | --- | --- | --- | --- |
> | no `Cache-Control` | 8 | 8 | **0** |
> | with `Cache-Control` | 8 | 8 | **0** |
>
> Chromium's in-page memory cache already answers a remount regardless
> of headers, so a row scrolling back into view was never re-fetching
> within a session. The header is still correct and still worth having —
> it earns a *disk* cache hit across app restarts — but it did not fix
> the reported flash. **B10 is what fixes that.**

The original reasoning, kept for the record:
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

### B10 — The first view of an image always flashes `✓ FIXED`

The residual symptom after B3/B4/B9: scrolling into a message whose
image hasn't been fetched yet shows an empty box for a frame or two.

Virtualization makes this structural. The `<img>` cannot exist before
its row mounts, so the fetch cannot start before then either.
`increaseViewportBy` mounts rows 600px early, but at any real scroll
speed that is a fraction of a second of lead — not enough for a round
trip.

Measured, with a handler given a 40 ms delay to stand in for a real
request:

| | paints synchronously on mount |
| --- | --- |
| cold — no prefetch | **false** — empty box for a frame |
| warmed via `new Image()` first | **true** — paints in the same frame |

**Fixed** by decoupling the fetch from the mount.
`attachmentPrefetch.ts` warms the previews for messages ±15 either side
of the visible range, driven off `rangeChanged`, so the bytes are in
cache before the row exists and the image paints in the frame it mounts.

The URL it warms comes from a `previewUrlFor()` helper now shared with
`ImageItem` — a prefetch that picked a different thumbnail size than
the component requests would be worse than none, doubling requests and
still missing.

**B11 closes the remaining gap.** Prefetching can't cover the first
screen of a channel or a fling that outruns the window; a placeholder
does, because it needs no request at all.

### B11 — ThumbHash placeholders `✓ IMPLEMENTED (server side uncompiled)`

The structural fix, and what Discord actually does: ship a ~25-byte
blurred preview *inside the attachment metadata*, so the box is never
empty regardless of what the network is doing. Prefetching races the
fetch; this removes the race.

Carried as a base64 `string` on `Attachment` rather than `bytes`, so it
stays opaque the whole way — the community server and the native client
move it as a plain string with no codec dependency, and only the
renderer decodes it. ~34 bytes on the wire instead of 25, which is not
worth a base64 implementation in two more languages.

Encoding is free: the uploader already decodes the bitmap to generate
thumbnails, so the hash comes off the same pixels. The blur is painted
as the wrapper's `background-image`, so the `<img>` simply draws over
it — no swap, no fade, nothing to time wrong.

**Verified** end to end in the renderer: a 400×260 test scene encodes to
28 base64 chars and decodes to a recognisable blur of the same image.

**Two caveats:**

1. **The C++ server changes are uncompiled.** There's no `cmake` on this
   machine. The DB layer, the `/attachments/init` parsing and both
   proto-fill sites are written to the existing patterns, and `protoc`
   validates the schema, but the server needs a real build.
2. **It only applies to new uploads.** The hash is computed client-side
   at upload time, so attachments already in a channel have no
   placeholder and keep the old flat fill. Backfilling would need image
   decoding in the C++ server, which is a much larger change than this
   was.

While wiring the DB column I also found that the `attachments_v4`
rebuild enumerates its columns explicitly, so it would have silently
dropped `placeholder` for anyone still on a pre-v4 schema — the exact
hazard its own comment warns about. Added there too.

**Postscript — the implementation shipped broken and why.** The hash was
empty for every *pasted or dragged* image. `probeMetadata` computed it
from `fetch(source.url)`, and `fetch` is governed by `connect-src`,
which lists `decibell-file:` but **not** `blob:` — so files picked from
disk worked and pasted ones silently didn't. Measured under that CSP:

| | |
| --- | --- |
| draw custom-scheme `<img>` → canvas | CLEAN |
| fetch `decibell-file:` | ALLOWED |
| draw `blob:` `<img>` → canvas | CLEAN |
| fetch `blob:` | **BLOCKED** |

Since drawing taints nothing, the encoder now reads pixels straight off
the `<img>` the upload path already decoded — no fetch, one less decode,
and no reason to widen the CSP.

Diagnosing it took a temporary probe that reported whether the field was
*absent* (stale native module), *present but empty* (nothing stored), or
*populated*. That three-way split found it in one round after three
rounds of fruitless code reading. Worth repeating for any
cross-stack field that arrives empty.

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

---

## 2026-08-15 round — decode-side fixes + instrumentation

Everything before this section targeted *the fetch* (bytes absent when
the row mounts). This round's code reading found the layer nobody had
looked at: **the decode**. Warm bytes still leave a decode to pay at
first paint, and `decoding="async"` — added in B3 — *guarantees* the
row paints before its pixels, one frame minimum, on every mount, cached
or not. That is a per-mount pop that survives every fetch-side fix, and
it matches the reported symptom ("attachments glitch into view while
scrolling") better than the empty-box theory did.

> **Verified by the user same day:** the glitch is *drastically reduced*
> after B12–B14. Small imperfections remain — see "Still open" below for
> the candidate mechanisms and what observation distinguishes them.

### Landed (typecheck + Vite build clean, user-verified live)

- **B12 — prefetch now pre-decodes.** `attachmentPrefetch.ts::warm`
  calls `img.decode()` on the detached warm image, so the decode lands
  in Chromium's decoded-image cache before the row exists, not in the
  row's first paint frame.
- **B13 — thumbnails present atomically.** `ImageItem` uses
  `decoding="sync"` for thumbnail variants (bounded ≤1280px, pre-decoded
  by B12, so sync cost is ~0 warm / a few ms cold) and keeps `"async"`
  only for full-size fallbacks, where a multi-MB original could block
  the scroll frame >100ms.
- **B14 — `memo(MessageBubble)` was defeated.** Both panels re-created
  their `onDelete` handler every render; it's a MessageBubble prop, so
  every panel render (each keystroke, each history-page prepend)
  re-rendered every visible bubble — a long frame exactly at the
  prepend moment mid-scroll. `useCallback` in `ChatPanel` and
  `DmChatPanel`; the handlers read active channel/peer from the store
  at call time so their identity never churns.
- **Suspect #1 instrumentation shipped** — `devRowHeightAudit.ts`,
  wired into MessageBubble's root (dev builds only). Any post-mount
  height settle logs `[row-audit] message <id>: height A → B`. One
  scroll session in dev now settles the "row height changes after
  mount" hypothesis with data.

### Closed by argument (verified against store code)

- **Suspect #2 (`reserveBox` 260×180 fallback) can't resize a mounted
  row.** `chatStore`'s `mergeMessage`/`prependHistory` never mutate or
  replace an existing message's object, so an attachment's
  width/height can't "arrive later" within a mounted row's life. The
  fallback box is constant for the attachment's lifetime; wrong-size,
  possibly, but stable.

### B15 — Page-boundary group-flip resize `✓ FIXED (user-verified same day: glitch gone)`

**The audit caught it same day.** The user's dev session logged exactly
one line:

```
[row-audit] message 1596: height 165.0 → 142.1 after first paint (Δ-22.9px)
```

Mechanism, confirmed against the code: the **oldest loaded message**
renders ungrouped — `shouldGroup` sees no predecessor — with the full
avatar/header row. When the next history page prepends, the same sender
usually continues above it, so the row re-renders *grouped* and shrinks
by the ~23px header (pt-2.5 + sender line vs py-px: matches Δ-22.9
exactly). `startReached` fires at the rendered edge, so this resize
landed precisely where the user was looking — once per 50-message page,
matching the "small imperfections" cadence. The flip itself is correct
data; the bug was *where* it happened.

**Fix:** eager pagination. Both panels now request the next page from
`rangeChanged` when the rendered window's top comes within
`HISTORY_EAGER_THRESHOLD` (25) messages of the boundary, so the flip
resolves on an unmounted or far-off-screen row where Virtuoso's
anchoring absorbs it. `startReached` remains as the forced fallback
(flings, lost responses) and keeps its retry semantics. The eager path
dedupes per boundary via `lastRequestedBeforeIdRef` because the history
response arrives via event, not the invoke promise — the in-flight
guard alone would re-request the same page in that gap. The audit now
tags settles ON-SCREEN vs off-screen; post-fix, boundary flips should
only ever log off-screen.

### Still open, in order

Suspect #1 (row height changing after mount) is **confirmed and has a
concrete instance fixed (B15)**. Remaining candidates for anything that
survives the B15 re-test:

1. **Re-run the audit.** Any remaining `ON-SCREEN` settle is the next
   B15-class bug; `off-screen` lines are benign by construction.
2. **Virtuoso height-estimation corrections** on the first upward pass
   over never-measured rows (~44px text vs 180–460px attachment rows;
   the size cache also dies with each channel-switch remount).
   *Signature: imperfections only on the first pass over a region;
   re-scrolling the same region is clean.* Confirmed from the
   installed 4.18.6 source this round: `rangeChanged` maps
   `listState.items` — the **rendered** range, `increaseViewportBy`
   included on both edges — so the saved scroll position already sits
   ~600px above the true viewport top, and raising the overscan
   blind widens that restore error. (`overscan` in contrast applies
   *directionally*, main = scroll direction.) The clean path is
   `getState()`/`restoreStateFrom` — pixel-exact restore that also
   revives measured sizes across the channel-switch remount — but the
   snapshot's size ranges are absolute-indexed, so it requires
   persisting `firstItemIndex` per channel instead of resetting to
   1e6 in `useVirtuosoPrepend`. Experiment first: paging, eviction,
   and messages-arriving-while-away all interact with it.
3. **Flings outrunning the prefetch window.** ±15 messages of lead
   evaporates on a fast fling; those images pop with only the blur /
   flat fill behind them. *Signature: imperfections only at high
   scroll velocity, and only on rows entering cold.*
4. **B6 remains unverified** against real video playback. *Signature:
   imperfections track the currently-playing video only.*
