# Wheel and key scrolling: the Chromium-matched spec

What the Electron client does when the message list is scrolled with a mouse wheel or a scroll key on Linux, measured
on 2026-11-26 and explained from Chromium's source, so the native client's list scrolls the same: same distance per
notch, same curve and duration, same behaviour when notches arrive mid-animation, same key steps.

- Measured with `tools/perf/electron-wheel-linux.py` (how it works: its header and `tools/perf/README.md`, "Wheel and
  key scrolling") on the isolated Electron client against the local stack, Daily Gaming #general, 1280×800 window,
  message list 762×574 CSS px, devicePixelRatio 1, compositor frames every 3.573 ms (280 Hz).
- Electron 33.4.11 = **Chromium 130.0.6723.191**, an X11 client under XWayland (KWin 6.8.3, Xwayland 24.1.15).
- Every number below is reproducible from the raw traces in `tools/perf/results/wheel-linux-*` (gitignored, on the
  Linux box), with `--reanalyze` for the analysis.

## 1. The answer

| | Value | Chromium source |
|---|---|---|
| Wheel notch | **120 CSS px** per detent. JS `wheel`: `deltaY` ±120, `deltaMode` 0 (pixels), `wheelDeltaY` ∓120 | `ui/events/x/events_x_utils.cc` `kWheelScrollAmount = 120` (X11; its comment cites crbug.com/1270089) |
| Curve | **cubic-bezier(0.42, 0, 0.58, 1)**, the CSS `ease-in-out`. Not smoothstep (fits 2.5× worse), not exponential | `cc/animation/scroll_offset_animation_curve.cc`: `kEaseInOut` = `EASE_IN_OUT` preset |
| Wheel duration | **D(Δ) = clamp(233.33 − 0.27778·\|Δ\|, 100, 200) ms**: 200 ms up to 120 px, 100 ms from 480 px (one notch: 200 ms) | `kInverseDelta`: 12 frames at 60 Hz up to 120 px, 6 frames from 480 px |
| Notch during an animation | target += 120 px (clamped to the scroll range); the curve restarts from the current position **with the current velocity**; new duration = min(D(remaining), 2.5 × remaining / velocity) | `UpdateTarget`, `VelocityBasedDurationBound`, `EaseInOutWithInitialSlope` |
| Notch at a boundary | target clamped to [0, max]; duration from the clamped distance (30 px left: 200 ms, not shorter); at the end: no movement | `LayerTreeHostImpl::ScrollAnimationCreate` → `ClampScrollOffsetToLimits` |
| Arrow keys | **40 px** | `cc::kPixelsPerLineStep = 40` |
| PageUp / PageDown, Space / Shift+Space | **⌊0.875 × visible height⌋** (502 px for a 574 px list). Not max(87.5 %, view − 40) on Linux | `ScrollableArea::PageStep`; `ScrollbarThemeAura` keeps `MaxOverlapBetweenPages() = INT_MAX` |
| Home / End | to the start / end of the scroll range | `kScrollByDocument` |
| Key duration | **150 ms, constant** (any distance, PageDown's 502 px too), same bezier, same retarget rule with D = 150 | `kConstant` = 9 frames at 60 Hz |
| Held key | each auto-repeat (KDE default: 600 ms delay, 25/s) adds one step to the target: ArrowDown held → 40 px per repeat, ~1000 px/s | Blink `ScrollAnimator::UserScroll` → `DesiredTargetOffset() + step` |
| Precise deltas (touchpad class) | **not animated**: the delta is applied in the next frame | `InputHandler::ShouldAnimateScroll`: `delta_granularity != kScrollByPrecisePixel` |
| Content inserted above (history page) | the running curve is translated by the anchoring adjustment (start and target), it keeps going | `ScrollOffsetAnimationCurve::ApplyAdjustment` |

Smooth scrolling is on: the app passes no `--enable/--disable-smooth-scrolling` (`electron-client/electron/main/index.ts`:
its feature switches concern codecs, VA-API, PipeWire, Ozone and the audio sandbox); Chromium enables the
scroll animator on Linux unless GTK reports `gtk-enable-animations=false` (`ui/gfx/animation/animation_linux.cc`,
`content/browser/web_contents/web_contents_impl.cc`). The list scroller (`RealMessageList`) is `overflow-y: auto`,
`scroll-behavior: auto` (no CSS smooth scrolling), `overflow-anchor: auto`, `overscroll-behavior: auto`, no snapping.
Neither Windows-personality feature (percent-based scrolling, impulse curve) is on: `kWindowsScrollingPersonality` is
disabled by default (`ui/base/ui_base_features.cc`), and the measured curve is the ease-in-out one.

## 2. Reference model

This is cc's algorithm with one simplification: **every input is handled at its own timestamp**. It is within 0.3–1.0
px rms (1.7 for a 20-notch burst) and ≤ 4.4 px at the worst frame of what Chromium drew in every measured sequence
(§5.6). Units: px, ms.

```text
B(x; y1)  = cubic-bezier(0.42, y1, 0.58, 1) evaluated at x    // y1 = 0: CSS ease-in-out; solve x(t) = x, return y(t)
B'(x; y1) = its slope dy/dx at x                               // x clamped to [0, 1]

D_wheel(Δ) = clamp(14 − |Δ|/60, 6, 12) / 60 × 1000            // = clamp(233.33 − 0.27778·|Δ|, 100, 200)
D_key(Δ)   = 150

state: resting position p; animation a = { from, to, t0, t_seg, end, y1, kind } or none

position(t):                                   // t: the frame's time (the time the frame is for)
    if a is none: return p
    if t − a.t0 ≥ a.end: p = a.to; a = none; return p
    u = t − a.t0
    if u ≤ a.t_seg: return a.from
    return a.from + B((u − a.t_seg) / (a.end − a.t_seg); a.y1) × (a.to − a.from)

velocity(t):                                   // px per ms, sign of travel
    u = t − a.t0
    return B'((u − a.t_seg) / (a.end − a.t_seg); a.y1) × (a.to − a.from) / (a.end − a.t_seg)

input(t, Δ, kind):                             // a notch (Δ = ±120 × notches) or a key step; t = event time
    if a is not none and t − a.t0 ≥ a.end: p = a.to; a = none
    if a is none:
        to = clamp(p + Δ, 0, max_scroll)
        if |to − p| ≤ 0.1: p = to; return                      // at the end already: nothing moves
        a = { from: p, to: to, t0: t, t_seg: 0, end: D_kind(to − p), y1: 0, kind }
        return
    new_to = clamp(a.to + Δ, 0, max_scroll)                   // targets accumulate
    if |new_to − a.to| < 0.01: return                          // clamped onto the same target: unchanged
    u   = t − a.t0
    cur = position(t);  v = velocity(t)
    rem = new_to − cur
    if |rem| < 0.01: a.to = new_to; a.end = u; a.t_seg = u; return
    d = D_kind(rem)
    if |v| ≥ 0.00001 and rem / v > 0: d = min(d, 2.5 × rem / v)    // moving towards it: don't crawl the last px
    if d < 10: a.to = new_to; a.end = u; return              // cc: duration < kEpsilon (0.01 s): jump to it
    s = clamp(v × d / rem, −1000, 1000)                       // keep the speed: initial slope of the new segment
    a = { from: cur, to: new_to, t0: a.t0, t_seg: u, end: u + d, y1: 0.42 × s, kind }
```

- Opposite direction (a reversal): `rem / v < 0`, so no velocity bound (d = D(rem)), and `s < 0`: the new segment first
  keeps moving the old way, stops and comes back (§5.4: up to +47 px of overshoot for a reversal 60 ms in).
- Precise input (`ScrollDelta::Pixels` from a touchpad or a smooth-scroll device): no animation. If one is running,
  cancel it at its current position, then add the delta to `p` (Chromium: a new scroll gesture aborts the running
  animation, `ScrollAnimationAbort` at every GestureScrollBegin in the traces, and precise updates scroll directly;
  Blink's `ScrollAnimator::UserScroll` cancels its own animation for precise deltas).
- Content inserted or removed above the viewport (an older page, a trim): shift `p`, `a.from` and `a.to` by the same
  amount; the animation keeps its timing.
- A key during a wheel animation (or a notch during a key animation) was not measured. By source, Blink's key
  animator does not see cc's wheel animation and starts its own from the current offset (`DesiredTargetOffset()` is
  the current offset when Blink owns no curve), so the wheel's remaining distance would be dropped. Retargeting the
  one animation by the key step with D_key is the simpler choice and differs only in that rare case.
- What cc does that the model leaves out (all < 1 frame, and why the model is not exact to the pixel): it creates the
  animation when the compositor thread handles the input (P = 1–5 ms after the event here), shortens a new animation's
  duration by that delay (`D − (P − E)`, the traced `Duration` is 198.2–199.4 ms for a notch), shows one frame
  interval of progress on the first frame ("start the animation one full frame in", `ScrollAnimationCreate`), and
  retargets at `local time of the current BeginFrame − (P − E)` with the frame's own time base. Taking the first
  frame's time minus one interval as `t0` (instead of the event time) brings a single notch to 0.3 px rms (from 0.6);
  for long retarget sequences it changes nothing (§5.6). Either is fine; the test vectors use `t0 = event time`.

## 3. Input mapping in the native client

What a notch is at each layer, on this machine (KWin 6.8, the mouse at its default scroll speed):

| Layer | One wheel detent |
|---|---|
| libinput | `scroll_value_v120` 120; `scroll_value` 15 (degrees, `MOUSE_WHEEL_CLICK_ANGLE` default) |
| KWin → `wl_pointer` (v8+) | `axis_source` wheel, `axis_value120` **120 × ScrollFactor**, `axis` **15 × ScrollFactor** (KWin multiplies both by the device's "Scrolling speed", `kcminputrc` `[Libinput][…] ScrollFactor`, default 1.0) |
| Mutter / Weston → `wl_pointer` | `axis_value120` 120, `axis` 10 |
| Xwayland 24.1 → X11 client | XI2.1 smooth-scroll valuator += `value120 / 120` (= 1.0; it uses `axis / 10` only without value120), plus emulated button 4 / 5 |
| Chromium X11 (the Electron client here) | valuator 1.0 × `kWheelScrollAmount` 120 = **120 px** (a core button 4 / 5: also 120) |
| Chromium ozone/Wayland 130 (not used here) | `axis / 10 × 120`: **180 px on KWin**, 120 on Mutter (ignores `axis_value120`) |
| Chromium Windows (the Qt spike's measurement) | 100 px (`WHEEL_DELTA` 120 → 3 lines × 33.3 px) |
| GPUI Wayland backend (vendored Zed) | `ScrollDelta::Lines(value120 / 120 × 3)` (`SCROLL_LINES = 3`) for a wheel source; `axis` is ignored for wheels |

So in the native client on Linux: **px = 40 × GPUI lines** (= `value120`, i.e. 120 per detent × the KWin scroll
factor), animated per §2; fractional `value120` from high-resolution wheels gives fractional notches, each animated
(Xwayland passes them on as fractional valuators, Chromium multiplies by 120). `ScrollDelta::Pixels` (finger and
continuous sources: GPUI passes `axis × 3`) is precise: apply it directly, no animation. On Windows the same model
applies with 100 px per notch (not re-measured here).

## 4. Keys in the Decibell client

Chromium's default key scrolling applies when the list holds focus or was the last thing clicked, but the app's
`useTypeToFocusComposer` (`electron-client/src/features/chat/useTypeToFocusComposer.ts`) listens on `window`
keydown first whenever no text field has focus:

- **ArrowUp** edits the latest own message (`preventDefault`): it never scrolls the list.
- **Space / Shift+Space** (and every printable key) focus the composer, so the space is typed there: they never scroll
  the list either.
- **ArrowDown, PageUp, PageDown, Home, End** are left to Chromium: they scroll (§1).

The measurements of ArrowUp and Space below were taken with that listener stopped (`--bypass-app-keys`) to have
Chromium's own numbers. To match the Electron client, the native list scrolls on ArrowDown / PageUp / PageDown /
Home / End and leaves ArrowUp and printable keys to the composer logic.

## 5. Measurements

Medians [min..max] over runs; "cc model" is the re-implementation of `ScrollOffsetAnimationCurve` driven by Chromium's
own traced processing times and BeginImplFrame receipts for that run (rms / worst frame, px). Positions are scrollTop,
which Chromium rounds to whole pixels here, so ~0.15 px rms / 0.5 px max is exact.

### 5.1 One notch

| | down (8 runs) | up (8 runs) |
|---|---|---|
| distance | 120 | −120 |
| traced cc `Duration` | 198.6 [198.2..199.4] ms (= 200 − processing delay 0.9–1.7 ms) | same |
| first visible pixel after the event | 9.8 [8.7..12.0] ms | 10.1 [9.1..12.4] ms |
| last change after the event | 191.1 [188.9..193.6] ms | 191.3 [187.9..193.8] ms |
| fit: ease-in-out bezier, free start + duration | D 198.7 ms, rms 0.27, max 0.52 px | D 198.7 ms, rms 0.27, max 0.51 px |
| fit: smoothstep, free | D 181.4 ms, rms 0.69, max 1.43 px | D 181.3 ms, rms 0.70, max 1.40 px |
| fit: exponential, free | τ 91.6 ms, rms 14.2, max 22.4 px | τ 91.6 ms, rms 14.2, max 22.5 px |
| cc model | rms 0.15, max 0.53 px | rms 0.15, max 0.51 px |

Five more runs without tracing: identical (bezier D 198.7 ms, rms 0.28). One notch down (run 1), position after the
event:

| t (ms) | measured (px) | cc model (px) | reference model §2 (px) |
|---:|---:|---:|---:|
| 1.9 | 0 | 0.08 | 0.02 |
| 16.2 | 2 | 1.91 | 1.54 |
| 30.5 | 6 | 6.33 | 5.61 |
| 44.8 | 13 | 13.48 | 12.37 |
| 59.0 | 23 | 23.24 | 21.73 |
| 73.4 | 35 | 35.54 | 33.68 |
| 87.6 | 49 | 49.43 | 47.30 |
| 101.9 | 64 | 64.21 | 61.97 |
| 116.3 | 79 | 78.77 | 76.58 |
| 130.5 | 92 | 91.75 | 89.78 |
| 144.8 | 103 | 102.65 | 101.02 |
| 159.1 | 111 | 110.96 | 109.74 |
| 173.4 | 117 | 116.54 | 115.76 |
| 187.7 | 119 | 119.45 | 119.12 |
| 202.0 | 120 | 120.00 | 120.00 |

### 5.2 Notches during an animation

3 runs each, all traced; every one reaches exactly n × 120 px.

| notches @ spacing | last change after 1st notch | traced cc segment ends (`Duration` / `UpdatedDuration`, ms after start) | cc model rms / max |
|---|---|---|---|
| 2 @ 30 ms | 193 ms | 199.2, 199.3 | 0.15 / 0.49 |
| 2 @ 60 ms | 228 ms | 198.9, 233.2 | 0.16 / 0.50 |
| 2 @ 120 ms | 300 ms | 198.7, 308.5 | 0.17 / 0.49 |
| 3 @ 30 ms | 198 ms | 198.9, 199.3, 202.9 | 0.13 / 0.49 |
| 3 @ 60 ms | 275 ms | 198.2, 232.3, 281.3 | 0.17 / 0.50 |
| 3 @ 120 ms | 422 ms | 198.3, 309.1, 432.5 | 0.19 / 0.51 |
| 5 @ 30 ms | 236 ms | 199.3, 198.4, 201.5, 213.9, 239.2 | 0.15 / 0.51 |
| 5 @ 60 ms | 402 ms | 199.2, 235.4, 284.0, 346.3, 411.7 | 0.18 / 0.50 |
| 5 @ 120 ms | 662 ms | 198.6, 306.8, 431.8, 551.4, 671.4 | 0.21 / 0.51 |

The model's own segment ends match the traced ones to ≤ 0.14 ms (e.g. 2 @ 60: 198.88 / 233.03 against 198.87 /
233.17). No cap on accumulated distance and no acceleration: the target just grows by 120 per notch; quick notches
end *sooner* per px because D shrinks with the remaining distance and the velocity bound (5 @ 30 ms: 600 px in
236 ms). Two notches 60 ms apart (run 1):

| t (ms) | measured | cc model | reference |
|---:|---:|---:|---:|
| 23.9 | 4 | 3.78 | 3.41 |
| 45.3 | 13 | 13.45 | 12.66 |
| 66.7 | 29 | 29.29 | 28.00 |
| 88.1 | 53 | 53.08 | 51.24 |
| 109.6 | 86 | 85.64 | 83.38 |
| 131.0 | 125 | 124.73 | 122.31 |
| 152.5 | 165 | 165.44 | 163.25 |
| 174.0 | 200 | 200.53 | 198.90 |
| 195.3 | 225 | 224.82 | 223.86 |
| 216.8 | 238 | 237.56 | 237.21 |
| 238.2 | 240 | 240.00 | 240.00 |

### 5.3 Sustained wheel

20 notches 40 ms apart, 3 runs: 2400 px, steady speed **3000 px/s** (2993–3007 between 400 and 700 ms, i.e. exactly
the input rate: no acceleration, no cap), the position trails the accumulated target by 215–290 px (2–2.4 notches,
oscillating with the notch phase), and settles 146 ms after the last notch (last change at 906 ms). cc model rms 0.23,
max 0.57 px; segment ends grow from 198.6 to 912.7 ms in ~40 ms steps.

### 5.4 Reversal

| pattern | peak overshoot in the old direction | back at the start | cc model rms / max |
|---|---|---|---|
| down, up 30 ms later | +19 px at 80–89 ms | 214 ms | 0.15 / 0.49 |
| down, up 60 ms later | +46–49 px at 111–117 ms | 250 ms | 0.15 / 0.50 |
| down, up 100 ms later | +87 px at 143–147 ms | 294 ms | 0.17 / 0.49 |
| down, up 150 ms later | +116 px at 177–183 ms | 343 ms | 0.18 / 0.50 |
| down, down 40 ms, up 80 ms | ends at +120 px (no overshoot) | 163 ms | 0.14 / 0.49 |

Down then up 60 ms later (run 1): 0, 4, 13, 28, 40, 46, 45, 38, 28, 17, 8, 2, 0 px at 2.5, 24, 45, 67, 88, 110, 131,
153, 174, 196, 217, 238, 260 ms (cc model within 0.5 px, reference model within 1.0 px at every one).

### 5.5 Boundaries and history paging

- 30 px above the bottom, one notch down: 30 px over 197.7 ms (traced `Duration` 197.7–198.2: the inverse-delta
  maximum, as for any distance under 120 px), same bezier (rms 0.27 px), cc model rms 0.14 / max 0.50. At the bottom: nothing
  moves (2 runs). 200 px above the bottom, 3 notches 40 ms apart: 200 px, the third notch leaves the target and the
  traced duration unchanged (220.1 → 220.1 ms), as the model's "same target" rule says.
- The top of the loaded history, 4 notches up 60 ms apart from 1000 px: crossing the 800 px paging zone loads a 50-row
  page (+4167 px of content above); scroll anchoring moves scrollTop by the same amount and cc logs `scroll animation
  adjusted` (the curve translated). The content position stays continuous and the total is exactly 480 px (3 runs).
  At the adjustment the motion falls 7.6–9.0 px behind the uninterrupted curve and catches up over ~100 ms: the
  adjusted keyframe model keeps the start time but not the one-frame time offset, so the rest of the animation plays
  exactly one frame (3.573 ms) late. Modelling that slip brings the runs to 0.15–0.17 px rms (0.83 for one with an
  extra trim) from ~2. The page render also blocks the main thread for ~50 ms (rAF gaps); the compositor keeps
  animating through it. The native list should not copy the slip: translate the animation and carry on (§2).
- The real top of a channel's history (nothing more to page in) was not reached in a cheap way; by source it clamps at
  0 exactly as the bottom clamps at max.

### 5.6 How close the reference model (§2) is

Against every measured frame (rms / worst frame, px; medians over runs):

| | t0 = event time | t0 = first frame − 1 interval |
|---|---|---|
| one notch | 0.56 / 2.1 | 0.29 / 1.3 |
| 2 notches | 0.65 / 2.4 | 0.37 / 1.4 |
| 3 notches | 0.84 / 2.6 | 0.55 / 1.8 |
| 5 notches | 0.88 / 3.2 | 0.88 / 3.1 |
| reversals | 0.28–0.57 / 1.1–2.1 | 0.22–1.25 / 0.9–3.6 |
| 20 notches | 1.73 / 4.1 | 1.70 / 4.2 |
| keys (single, taps, held) | 0.13–0.29 / 0.5–1.6 | (keys start at the handling time in Blink: no frame offset) |

### 5.7 Keys

| key | distance | duration fit (bezier, free) | cc model rms / max |
|---|---|---|---|
| ArrowDown (4 runs) | 40 px | 149.8 ms, rms 0.26 | 0.13 / 0.50 |
| ArrowUp, app listener bypassed (3) | −40 px | 148.9 ms, rms 0.25 | 0.13 / 0.48 |
| PageDown (3) | 502 px | 150.0 ms, rms 0.29 (smoothstep: rms 2.4, max 4.9) | 0.19 / 0.98 |
| PageUp (2) | −502 px | 150.0 ms, rms 0.29 | 0.18 / 0.78 |
| Space, bypassed (3) | 502 px | 150.1 ms, rms 0.29 | 0.16 / 0.78 |
| Shift+Space, bypassed (3) | −502 px | 150.0 ms, rms 0.28 | 0.20 / 0.97 |
| End / Home (2 each) | to the end of the current range | 150 ms (first runs: 3410 px, rms 1.1; −7285 px, rms 2.2) | the list paged and trimmed during the later runs: distances not comparable |
| ArrowDown ×5, 60 ms apart (2) | 200 px | ends 396 ms after the first press | 0.24 / 1.1 |
| ArrowDown held 1.5 s (2) | 24 keydowns (1 + 23 repeats at 600 ms + 40 ms) = 960 px | ends 1633 ms after the press | 0.27 / 1.5 |
| PageDown ×3, 100 ms apart (2) | 1506 px | ends 351 ms after the first press | 0.37 / 1.3 |

PageDown (run 1): 0, 12, 44, 95, 164, 244, 325, 396, 451, 486, 501, 502 px at 2, 17, 31, 45, 60, 74, 88, 102, 117,
131, 145, 160 ms; the reference model gives 0.25, 12.21, 43.91, 95.43, 164.05, 244.19, 325.25, 396.70, 451.26,
485.99, 501.06, 502.00 px at the same times. A held key is a run of 40 px retargets, not a continuous velocity: each
repeat lands every 40 ms and adds 40 px, so the list moves at ~1000 px/s while the curve's retarget rule smooths the
steps.

### 5.8 Precise deltas

CDP `Input.dispatchMouseEvent` wheels carry `kScrollByPrecisePixel` deltas, the class Chromium gives touchpads
(ozone/Wayland `ScrollEvent`, X11 devices typed `TOUCHPAD`): one 120 px event moved 120 px in one frame (3 runs);
ten 12 px events 8 ms apart moved in steps that followed the input and ended ~100 ms after the first (2 runs). No
curve.

### 5.9 Noise

The box was shared with agents compiling and another agent measuring the GPUI client during the session (load average
0.4–3.5). In-motion rAF gaps: 0 in most runs, up to 3 (single notch) and 16 (the history-paging run, a 50 ms main-
thread stall rendering the new page); the model is evaluated at the frames that were drawn, so gaps do not bias it.
Later key runs had the window on the 180 Hz output, where rAF ran at 3.6 / 7.1 ms while cc's frames stayed at
3.573 ms; the model ticks on cc's frames there. Five key runs lost their keys to another window taking focus and are
reported as such, not counted. Of the 54 wheel runs traced with frame receipts (history paging aside), 53 fit the cc
model within 0.6 px at every frame (rms 0.13–0.23); the outlier (2 notches with rAF at half rate on the 180 Hz output,
rms 1.15, max 3.8 px) is cc's frame bookkeeping under throttled frames.

## 6. Chromium source (130.0.6723.191)

- `cc/animation/scroll_offset_animation_curve.cc`: `kDurationDivisor 60`, `kConstantDuration 9`,
  `kInverseDeltaRampStartPx 120`, `kInverseDeltaRampEndPx 480`, `kInverseDeltaMinDuration 6`,
  `kInverseDeltaMaxDuration 12`, `kEpsilon 0.01`; `EaseInOutSegmentDuration` (duration − `delayed_by`),
  `EaseInOutBoundedSegmentDuration`, `VelocityBasedDurationBound` (`(new_delta / velocity) × 2.5`, ∞ when negative),
  `EaseInOutWithInitialSlope` (`CubicBezier(0.42, slope × 0.42, 0.58, 1)`, slope clamped ±1000), `UpdateTarget`,
  `GetValue`, `ApplyAdjustment`. Impulse constants (0.25 / 0 / 1, 200–500 ms, 1.5 ms per px) exist but are off.
- `cc/animation/scroll_offset_animation_curve_factory.cc`: mouse wheel → `kInverseDelta`, keyboard → `kConstant`,
  programmatic → `kDeltaBased` (√distance frames, ≤ 0.7 s), autoscroll → linear; impulse only with
  `kWindowsScrollingPersonality` (`cc/base/features.cc`, default off).
- `cc/animation/scroll_offset_animations_impl.cc`: `MouseWheelScrollAnimationCreate`, `ScrollAnimationUpdateTarget`
  (new target = old target + delta, clamped to [0, max]; `t = local time at CurrentBeginFrameArgs − delay`),
  `ScrollAnimationApplyAdjustment` (the anchoring path; copies the start time but not the one-frame time offset).
- `cc/trees/layer_tree_host_impl.cc` `ScrollAnimationCreate`: target clamped, `animation_start_offset = one frame
  interval`.
- `cc/input/input_handler.cc`: `ScrollLatchedScroller` (create or update), `ShouldAnimateScroll` (everything but
  precise pixels when `enable_smooth_scroll`).
- `third_party/blink/renderer/platform/widget/input/input_handler_proxy.cc`: `delay = Now − event timestamp` passed
  as `delayed_by`.
- `components/input/mouse_wheel_event_queue.cc`: the GestureScrollUpdate carries the wheel event's timestamp.
- `ui/events/x/events_x_utils.cc`: `kWheelScrollAmount = 120`, core buttons 4–7 and XI2 scroll valuators
  (`valuator delta × 120`), `TimeTicksFromXEventTime`; `ui/events/devices/x11/device_data_manager_x11.cc`
  (`ExtractAndUpdateScrollOffset`: valuator delta / increment; touchpads = XI device type `TOUCHPAD`).
- `ui/events/blink/web_input_event.cc` `MakeWebMouseWheelEventFromUiEvent`: delta = offset in DIP, `kScrollByPixel`
  unless `EF_PRECISION_SCROLLING_DELTA`; `ScrollEvent` (touchpads) → `kScrollByPrecisePixel`.
- `ui/ozone/platform/wayland/host/wayland_pointer.cc` `OnAxis` (`value / 10 × 120`; `axis_value120` and
  `axis_discrete` not implemented), `wayland_event_source.cc` (wheel source → `MouseWheelEvent`, finger / continuous →
  `ScrollEvent`).
- Blink keys: `core/input/keyboard_event_manager.cc` (`MapKeyCodeForScroll`, `DefaultArrowEventHandler`,
  `DefaultSpaceEventHandler` on keypress), `core/input/scroll_manager.cc` `LogicalScroll` (starts at the focused node,
  else the last mouse-pressed node), `core/scroll/scrollable_area.cc` (`LineStep` 40, `PageStep`),
  `core/scroll/scroll_animator.cc` (`UserScroll`, `WillAnimateToOffset`, keyboard → `kKeyboard` curve, start time =
  main-thread `Now`), `cc/input/scroll_utils.h` (`kPixelsPerLineStep 40`, `kMinFractionToStepWhenPaging 0.875`),
  `core/scroll/scrollbar_theme.h` (`MaxOverlapBetweenPages` = `INT_MAX`; not overridden by `ScrollbarThemeAura`).
- `ui/gfx/geometry/cubic_bezier.cc`: the bezier solver (spline guess, 4 Newton steps, bisection; ε 1e-7).

Measurement and source agree everywhere they overlap; the measured durations are 200 ms minus the traced processing
delay to within 0.1 ms. The only place they part is §5.5's anchoring hitch, which the source explains (the adjusted
keyframe model has no time offset).

Outside Chromium: KWin 6.8.3 `src/backends/libinput/events.cpp` (`scrollValue` / `scrollValueV120` × the device's
`scrollFactor`), `src/wayland/pointer.cpp` `sendAxis` (`axis_value120` + `axis` for v8 clients); Xwayland 24.1
`hw/xwayland/xwayland-input.c` (`dispatch_scroll_motion`: `value120 / 120.0`, else `axis / 10`; `pointer_handle_axis_source`
ignores the source; `add_device` types every device with its driver name, never `TOUCHPAD`).

## 7. System settings at measurement

- KDE Plasma 6.8.3 Wayland, KWin 6.8.3, Xwayland 24.1.15 (`kwinrc [Xwayland] Scale=1`), outputs DP-1 2560×1440 @
  279.96 Hz (primary, scale 1) and DP-2 1920×1080 @ 180 Hz.
- `kcminputrc`: `[Libinput][14096][21510][Pulsar Pulsar 8K Dongle]` `NaturalScroll=false`, no `ScrollFactor` (1.0,
  "Scrolling speed" at its default); no `[Keyboard]` group, so KDE's defaults: repeat delay 600 ms, rate 25/s (the held-
  key run uses those).
- `~/.config/gtk-3.0/settings.ini` `gtk-enable-animations=true` (Chromium's scroll animator on).
- The injected events bypass KWin, so they are the 1.0 scroll factor by construction; a real detent is ×ScrollFactor
  (§3).

## 8. Against the Windows measurement (`qt-client/HANDOFF.md` §2d)

- **Distance**: Windows 100 px per notch, Linux 120 px.
- **Curve**: Windows was read as "smoothstep over ~175 ms". On Linux the same notch is the ease-in-out bezier over 200 ms;
  smoothstep's best fit is 181 ms with 1.4 px error, but only with its start 8.7 ms later than the bezier's: a
  smoothstep fit of that bezier, which is most likely what the Windows reading was. cc runs the same code on Windows
  (the impulse curve is off by default) and D(100 px) is 200 ms there too, so the Windows notch is most likely the same
  200 ms bezier; not re-measured. The Qt spike's `SmoothWheel` (cubic Hermite over 175 ms from the input) differs from
  the bezier over 200 ms from the input by up to 10.7 px on a 100 px notch (it leads, then finishes 25 ms early).
- **Retarget**: "adds 100 px and restarts from the current position keeping the speed" holds, but the new segment's
  duration is not a fixed 175 ms: it is D(remaining distance) (shorter as more distance piles up) bounded by 2.5 ×
  remaining / speed.
- **Keys**: 150 ms constant, not the wheel's curve duration. Page step on Linux is ⌊87.5 %⌋ of the view, not max(87.5 %,
  view − 40) (534 px here instead of the measured 502).
- **Touchpad**: precise deltas pass through unanimated (as in the Qt spike).

## 9. Test vectors

For the native list's self-test: feed the inputs into the animator with a synthetic clock, sample `position(t)` at the
given times, compare within **0.05 px** (these are the §2 model's own values, t0 = event time; the bezier solved to
1e-9). Inputs are `Δ@t` in px and ms; `p0` the resting position, `[0, max]` the scroll range; inputs are applied in
time order, and an input at exactly a sample time is applied before that sample.

| vector | inputs | p0, range | expected position at t (ms): px |
|---|---|---|---|
| W1 one notch down | +120@0 | 1000, [0, 10000] | 0: 1000.00, 10: 1000.58, 25: 1003.73, 50: 1015.50, 75: 1035.13, 100: 1060.00, 125: 1084.87, 150: 1104.50, 175: 1116.27, 199: 1119.99, 200: 1120.00, 250: 1120.00 |
| W2 one notch up | −120@0 | 1000, [0, 10000] | 25: 996.27, 50: 984.50, 100: 940.00, 150: 895.50, 200: 880.00 |
| W3 two notches 60 ms apart | +120@0, +120@60 | 1000, [0, 10000] | 30: 1005.43, 60: 1022.49, 90: 1053.72, 120: 1101.65, 150: 1158.61, 200: 1227.78, 250: 1240.00 |
| W4 three notches 30 ms apart | +120@0, +120@30, +120@60 | 1000, [0, 10000] | 30: 1005.43, 60: 1029.27, 90: 1087.03, 120: 1183.40, 150: 1286.03, 180: 1347.94, 203: 1360.00 |
| W5 five notches 30 ms apart | +120@0, 30, 60, 90, 120 | 1000, [0, 10000] | 60: 1029.27, 120: 1189.08, 150: 1327.42, 180: 1474.26, 210: 1572.64, 239: 1600.00 |
| W6 reversal | +120@0, −120@60 | 1000, [0, 10000] | 30: 1005.43, 60: 1022.49, 90: 1040.16, 115: 1045.92, 150: 1039.52, 200: 1014.83, 260: 1000.00 |
| W7 30 px from the end | +120@0 | 1000, [0, 1030] | 50: 1003.87, 100: 1015.00, 150: 1026.13, 200: 1030.00 |
| W8 twenty notches 40 ms apart | +120 every 40 ms from 0 (20) | 1000, [0, 100000] | 100: 1087.62, 200: 1413.68, 400: 2029.53, 600: 2629.23, 760: 3108.85, 800: 3228.74, 900: 3398.61, 1000: 3400.00 |
| K1 ArrowDown | +40@0 (key) | 1000, [0, 10000] | 25: 1002.25, 50: 1009.27, 75: 1020.00, 100: 1030.73, 150: 1040.00 |
| K2 PageDown, 574 px view | +502@0 (key) | 1000, [0, 10000] | 25: 1028.18, 50: 1116.35, 75: 1251.00, 100: 1385.65, 125: 1473.82, 150: 1502.00 |
| K3 ArrowDown held | +40@0, then +40@600, 640, 680, 720, 760 (key) | 1000, [0, 10000] | 100: 1030.73, 150: 1040.00, 620: 1041.42, 700: 1078.88, 800: 1176.89, 900: 1239.63 |

Bezier spot values for the solver, `B(x; 0)`: 0.05 → 0.0048, 0.10 → 0.0197, 0.20 → 0.0817, 0.25 → 0.1292, 0.30 →
0.1874, 0.40 → 0.3319, 0.50 → 0.5000, 0.60 → 0.6681, 0.70 → 0.8126, 0.80 → 0.9183, 0.90 → 0.9803, 0.95 → 0.9952.
`D_wheel`: 30 → 200, 120 → 200, 180 → 183.33, 240 → 166.67, 300 → 150, 360 → 133.33, 480 → 100, 600 → 100 ms.

Against the real thing, a native implementation of §2 should land within ~1 px rms and ~4.5 px at the worst frame of
the measured Chromium traces (§5.6); `tools/perf/electron-wheel-linux.py` is the harness to re-measure.

## 10. Not verified here

- **A physical wheel.** The injected core button 4 / 5 takes Chromium's X11 path from `ui::MouseWheelEvent` on, the
  same as a real detent (by source: Xwayland's valuator 1.0 × 120 = the button's 120, both `kScrollByPixel`, no phase);
  the KWin → Xwayland part and the scroll factor are by source only. XTest could not be used (KWin's input-emulation
  prompt for the owner), so the real-hardware input path was never driven.
- **A touchpad.** Precise deltas are measured unanimated (CDP), the class Chromium gives touchpads on ozone/Wayland and
  on X11 `TOUCHPAD` devices. But under XWayland a touchpad reaches Chromium as smooth-scroll valuators on
  `xwayland-pointer` (never typed `TOUCHPAD`), i.e. as a mouse wheel, so the Electron client here most likely animates
  touchpad scrolling with the wheel curve (and at 12 px per `wl_pointer.axis` unit): an XWayland artefact, not
  something to copy. The native client passes precise input through (§2), like Chromium on Wayland and Windows.
- **The very top of a channel** (no more history): clamps at 0 by source, symmetric to the measured bottom.
- **Windows** values in §8 come from the Qt spike's notes and the source, not from this harness.
- **High-resolution wheels** (fractional `value120`): by source each fraction is a separate animated retarget.
- Keyboard auto-repeat is KDE's default (600 ms / 25 per s) simulated by the tool; the X server's own repeat was not
  read.
