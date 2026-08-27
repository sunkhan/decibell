# Message list: Virtuoso jump postmortem + real-DOM (Discord-style) migration plan

**Status:** implemented 2026-08-27 (`RealMessageList.tsx`, both panels, on by default via
`USE_REAL_LIST`; Virtuoso kept as the fallback until DMs are live-verified, then deleted).
Deviations from §5 found in build/live test: trims cut by pixel distance, not count (a count
cut can land inside the paging zone and ping-pong); `overflow-anchor` stays auto — Chromium
anchors prepends because a programmatic `scrollTop` write cancels the compositor wheel
animation, the list's math is the residual; positions are `{anchorId, offset}` rather than
raw px; jump landings are centered. Feature-log entry in
`docs/reviews/2026-08-23-community-server-review.md`.
**Scope:** renderer-only (`ChatPanel.tsx`, `DmChatPanel.tsx`, a new shared list component,
small store additions). No server, proto, or native changes required — that side is done.

## 1. Why this document exists

Implementing Discord-style windowed jump-to-message (2026-08-25) surfaced seven rounds of
landing/scroll bugs, every single one rooted in the same property of react-virtuoso: **any
placement or animation that depends on the height of a row that hasn't mounted yet is
computed from an estimate, and estimates leak into visible behavior.** The feature works
now, but the fix stack is a tower of compensations. Discord avoids this entire bug class
architecturally; we now have every piece of infrastructure their approach needs.

## 2. The seven rounds (what happened, root cause, fix)

| # | Symptom | Root cause | Fix (commit theme) |
|---|---------|-----------|--------------------|
| 1 | First-click jump landed wrong; second click fine | `scrollToIndex({behavior:"smooth"})` eases toward a pixel computed **once** from estimated row heights; rows measuring mid-flight shift the true destination | (superseded) |
| 2 | Attachment-scroll glitch regression while paging | `jumpToMessage` had `messages` in its `useCallback` deps → new identity per prepend → broke `memo(MessageBubble)` → full re-render of every visible bubble mid-scroll | Identity-stable callback: read the store at call time, deps `[flash]` |
| 3 | Smooth jump fought back-and-forth during animation | Eager paginator prepended a page mid-animation; every index shifted under the in-flight scroll while prepend-anchoring pulled the other way | (superseded: paging pause window) |
| 4 | Still mis-landed after animation | Fixed-delay settle-snap raced row measurement | (superseded) |
| 5 | rAF loop re-targeting the row's live rect still overshot | Tall code-block rows mounting mid-flight re-anchor the scroller **under** the animation | (superseded) |
| 6 | Even the instant remount mis-landed near code blocks | `align:"center"` picks the topmost row by walking up from the target with **estimated** heights and anchors that row | `align:"start"` — anchor the target's own top edge (no estimates involved) |
| 7 | One case left: code blocks **below** the target | Rows below are estimates too; under-estimation makes Virtuoso think content below is less than a viewport → it **clamps the scroll upward** instead of honoring `initialTopMostItemIndex`, and the clamp sticks once rows measure tall | ~1s post-jump assertion window: `totalListHeightChanged` re-pins the target (`jumpAssertUntilRef`) until real heights land |

**Final working design (current `main`):** every jump — near, far, or unloaded → around-window
fetch — lands via an exact remount (`key` epoch + `initialTopMostItemIndex` `align:"start"`),
plays a 0.26s CSS transform arrival slide (`jumpArriveUp/Down`) + highlight flash, and is
guarded by the 1s landing assertion. Alignment note: **do not switch back to
`align:"center"`** on Virtuoso — the assertion window would mostly hide the drift, but
center re-introduces dependence on estimated heights above the target and will visibly
re-settle in heavy code-block channels. With a real-DOM list (below), centering becomes
trivially exact and can be revisited for free.

## 3. How Discord avoids this bug class entirely

Discord's message list is **not virtualized by estimation**. The DOM contains only the
loaded slice (~50–100 messages) as real elements in a plain scroller:

- Scrolling toward an edge fetches the next page **and trims the far end**, so DOM size is
  bounded regardless of channel size — a 10M-message channel costs the same DOM as a new one.
- Media reserve exact dimensions from metadata before load, so row heights never shift.
- Therefore every height in the list is *real*; placement (jump landing, centering,
  anchoring) is simple arithmetic over true values. The bug class we spent seven rounds on
  cannot occur.

## 4. Architecture we already have (the prerequisites are DONE)

Server / wire (community SQLite + central Postgres):
- Windowed history: `around_id` (context window), `after_id` (downward page), `before_id`
  (upward page), `has_more` + `has_more_after`, echoed request mode for client routing.
- Bounded window fetches with target-included stitching (`fetch_messages_around`,
  `fetchDmHistoryAround`, `*_after` variants). e2e-covered (231 checks).
- Server-embedded reply previews (`reply_to_sender`/`reply_to_content` on broadcast +
  history) — reply rendering has **no dependency on what's loaded**, so aggressive DOM
  trimming can't break previews.

Client stores:
- Windowed slice semantics both directions: `hasMoreHistory` + `hasMoreAfter`,
  `setChannelWindow`/`appendNewer`/`resetChannelForJump` (+ `setDmWindow`/`appendNewerDm`/
  `resetDmForJump`), live-append drop while windowed, jump-to-present reload.
- Prepend dedup by id; identity-stable bubble callbacks (memo-safe).

Client UI already in place and reusable as-is:
- Jump-to-present pill (windowed + scrolled-up triggers), arrival-slide keyframes,
  highlight flash, per-channel scroll save/restore concept, attachment prefetch,
  reserved attachment boxes (width/height metadata → `attachmentSizing`).

**The missing piece is only the list component itself.**

## 5. Migration plan: replace Virtuoso with a real-DOM sliding window

New shared component (e.g. `features/chat/RealMessageList.tsx`) used by both panels:

1. **Rendering** — plain `overflow-y:auto` div rendering the loaded slice directly
   (`messages.map(...)` → `MessageBubble`). Cap the slice at ~150 rows.
2. **Trimming** — when a page-in pushes the slice over the cap, drop rows at the far end
   and set the matching flag (`hasMoreHistory` / `hasMoreAfter`) to true so pagination can
   re-fetch them. Needs two small store setters: `trimHead` / `trimTail` (both panels'
   stores). Use hysteresis (trim to ~120 when crossing 150) to avoid thrash.
3. **Prepend anchoring** — manual and exact with real DOM: in a layout effect, record
   `scrollHeight` before the prepend commit and add the delta to `scrollTop` after.
   (This replaces `useVirtuosoPrepend`/`firstItemIndex` entirely.)
4. **Pagination triggers** — two `IntersectionObserver` sentinel divs (top/bottom) replace
   `startReached`/`endReached`/`rangeChanged`-eager. Keep the existing in-flight +
   boundary-dedup guards verbatim.
5. **Bottom-follow** — if the user is at (near) the bottom, pin `scrollTop` on append;
   otherwise leave the view and let the pill show. The `atBottom`/pill logic transfers.
6. **Jump** — swap in the around-window slice (existing store path), then
   `rowRef.scrollIntoView({block:"start"})` (or exact `scrollTop` math) on the target row —
   exact by construction, since all heights are real. Keep the arrival slide + flash.
   Centered landings become safe to offer here.
7. **Scroll restore** — save raw `scrollTop` px per channel/peer (simpler and more precise
   than the current index-based restore).
8. **Prefetch** — drive `prefetchAround` from the sentinel/scroll position instead of
   `rangeChanged`.
9. **Unchanged** — `MessageBubble`, grouping rules, composer, reply/edit/delete flows,
   stores' message ingestion, all server code.

Perf notes: 150 real rows with reserved-size media is comfortably within Chromium budget
(this is literally Discord's model); the memo work from round 2 already prevents re-render
storms; content-visibility (`content-visibility:auto` on rows) is an optional cheap win for
very tall slices.

Suggested order: build `RealMessageList` behind a temporary toggle in `ChatPanel`, verify
scroll/prepend/jump/trim parity in channels, port `DmChatPanel`, then delete the Virtuoso
paths (`useVirtuosoPrepend`, epoch-key remount, `jumpAssertUntilRef`, landing assertion)
and the react-virtuoso dependency.
