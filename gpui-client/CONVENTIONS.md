# Conventions for code in `gpui-client/`

Read with `HANDOFF.md` (building, running) and `docs/gpui-notes.md` (GPUI at the pin, with source lines).
The client exists to be lean: every rule here serves low memory, zero frames at idle, fast startup or a
small binary. Measure what you add (`HANDOFF.md` §2, `tools/perf/`).

## Crates

| Crate | Holds | May depend on |
|---|---|---|
| `decibell-theme` | The design tokens, generated from `globals.css`. Pure data, no GPUI. | nothing |
| `decibell-harness` | `Screen`, `SelfTest`, `TestCx`, `perf` (frame counters, first frame). | gpui |
| `decibell-ui` | The design system on GPUI: `Theme`, fonts, `Label`, `Icon`, window frame, title bar, shared components. No feature logic, no stores. | theme, harness |
| service crates (`decibell-attachments`, `decibell-unfurl`, …) | What Electron's main process did. No GPUI. | — |
| feature crates (one per area) | Views and their state. | ui, harness, theme, the services they use |
| `decibell-app` | The binaries. Boot, the main window, the root view, the registry of screens and self-tests. | everything |

- GPUI comes only through the workspace: `gpui.workspace = true` (and `gpui_platform.workspace = true` in the
  app). Never a version, a git URL or `default-features`. Never another Zed crate: most are GPL.
- `test-support` (GPUI's test platform) is a dev-dependency feature or sits behind `decibell-app`'s `capture`
  feature. It must not reach the `decibell` binary: don't add it to `[dependencies]`.
- New crates use `version.workspace = true`, `edition.workspace = true`, `publish.workspace = true`, and are
  added to `members` in `Cargo.toml` (an explicit list: a half-written crate must not break everyone's build,
  so add the line when the crate compiles).
- Every thread you spawn gets a name (`std::thread::Builder::name`): `tools/perf/measure.sh` reports unnamed
  threads beside the GPU driver's.

## Theme

- A view reads the theme with `cx.theme()` (`use decibell_ui::ActiveTheme as _;`):
  `theme.color_bg_primary` (an `Hsla`, the CSS `--color-bg-primary`), `theme.radius_md` (`Pixels`),
  `theme.shadow_float.clone()`, `theme.scaled.text_body` (px at the user's text size), `theme.palette.<token>`
  for raw numbers. Field names are the CSS custom properties snake-cased, so a token greps both ways.
- Inside the chrome (title bar, server bar, sidebars, image viewer: `.chrome-scope` in the CSS) read
  `theme.chrome.<token>`. It differs from the base scope only in console-split.
- **No colour, radius, shadow or font that is not a token.** No `rgb(0x…)`, no `px(6.)` radius, no
  `shadow_md()`. Spacing uses GPUI's Tailwind-scale helpers (`p_4`, `gap_2`, `h_8`: 4 px steps, as in the TSX)
  or `px(n)` where the TSX has an arbitrary `[n px]`. Two exceptions exist and are commented: the window's own
  shadow (`window_frame.rs`) and the white of the close button's hover (`hover:text-white`).
- Converting tokens costs nothing at paint: `Theme` holds `Hsla` / `BoxShadow` values made once per theme
  switch. Don't call `decibell_ui::hsla` / `box_shadows` in `render`, except for a palette that is not the
  active one (a theme preview).
- `Theme::set(cx, id)` switches and repaints every window; nothing else is needed in views.
- CSS → GPUI differences already handled in `decibell-ui`: a CSS blur radius is twice GPUI's `blur_radius`;
  CSS paints the first shadow layer on top, GPUI the last. Not handled anywhere: GPUI has no letter-spacing
  (`tracking_*` tokens), no percentage radii (use `theme.avatar_radius(size)`), a spread shadow keeps the
  element's corner radius instead of growing it, and `overflow_hidden` clips to the rectangle, not to rounded
  corners (round the child too).

## Text

- All UI text is a `decibell_ui::Label`:
  `Label::new("…").font(FontRole::Channel).size(12.).weight(500.).color(theme.color_text_secondary)`.
  Never a bare `&str` child for user-visible text and never `.font_family("…")` in a view: the element behind
  `Label` will change (`decibell-text`), and family names live in one place (`decibell_ui::fonts`).
- Size: a role from `theme.scaled` where the TSX uses `text-body` / `text-meta` / …; a literal where it has
  `text-[13px]`. Line height defaults to 1.5 × size (what the renderer inherits); pass the px value when the
  TSX sets one.
- Weights that exist: 400, 500, 600, 700 (Mona Sans wordmark: 600 only). Others snap to the nearest face.
  There is no italic: GPUI does not synthesise a slant, italic text is drawn upright for now.
- Adding a weight or family: `tools/gen-fonts.py` (`FAMILIES`), run `tools/gen-fonts.sh`, commit
  `assets/fonts/static/` and `crates/decibell-ui/src/fonts_generated.rs`. The unit tests and the `boot`
  self-test then check the new face resolves to itself.

## Icons

- `Icon::new("icons/lucide/settings.svg").size(px(16.)).color(theme.color_text_secondary)`. The colour
  defaults to the inherited text colour; `.stroke_width(1.5)` matches lucide-react's `strokeWidth`;
  `.group_hover("name", colour)` follows a hovered ancestor that called `.group("name")` (an icon cannot
  inherit a hover colour: its colour is fixed when its parent renders).
- Adding an icon: put the SVG under `assets/icons/` (lucide ones through `tools/fetch-assets.mjs`, the
  renderer's hand-written ones as files, e.g. `assets/icons/window/`). `decibell-ui`'s build script embeds
  every `.svg` there; nothing else to register. SVGs are drawn as one colour (alpha mask + tint).

## Element ids

- `.id("…")` is needed for anything interactive or scrollable (`on_click`, `overflow_y_scroll`, hover
  groups, focus). Ids must be unique among siblings and stable across frames: a literal for singletons, the
  model's key for repeated items (`("message", message_id)`, never the index in a list).

## Frames and wakeups (the idle rules)

The targets: 0 frames drawn while nothing on screen changes, no timer of ours firing at idle, one UI wake
per burst of events. The `idle-frames` self-test fails the build's bar when a screen leaks frames or wakeups.

- A frame happens only because state changed: call `cx.notify()` on the entity whose state changed, nothing
  else. Never `window.refresh()` from a view (it redraws everything and bypasses view caches), never
  `window.request_animation_frame()` unconditionally.
- **No polling timers.** A timer exists for a named, live reason (a countdown on screen, a toast's lifetime)
  and its `Task` is dropped when the reason ends. Hold tasks in the view so they die with it.
- **Animations run only while visible and live.** Stop them when the element leaves the screen, the window
  is hidden (`observe_window_visibility`) or the cause ends. An `svg()` `TimedTransformation` re-presents
  without re-rendering: prefer it for spinners. A repeating `with_animation` never stops by itself.
- **One cached view per pane.** A pane (channel list, message list, member list, composer, a settings tab)
  is its own entity, drawn with `entity.clone().cached(StyleRefinement::default().size_full())` as
  `root.rs` does for the content. A `notify()` in one pane then re-renders that pane only; the others
  replay their last paint. Keep `render` free of side effects so replay is safe.
- Read other entities in `render` only if their change must redraw you: reading subscribes the window.
- Events from the core or any other thread reach the UI through one channel drained per wake
  (`docs/gpui-notes.md` §4.3), never one task per event.
- Nothing on a hot path (scrolling, a frame of video, typing) allocates per frame or per item when it can
  be kept: precompute in the model, not in `render`. `SharedString::new_static` for literals.
- No hover handler that redraws (`on_mouse_move(|…| window.refresh())`): use `.hover(…)`, `.cursor(…)`,
  `.group_hover(…)`, which cost a frame only when the hover state changes.

## Screens (captures) and self-tests

- A crate with something to show exports `pub fn screens() -> Vec<decibell_harness::Screen>`; one with
  behaviour to check exports `pub fn selftests() -> Vec<decibell_harness::SelfTest>`. Add the crate's call to
  `crates/decibell-app/src/registry.rs`. Names are lower-case with dashes and unique (a test enforces it).
- **A screen** builds a view from fixture state it creates itself: no network, no config file, no clock, no
  random numbers, so the same screen, theme, size and scale is the same picture. See `decibell_ui::screens`.
  Capture it: `cargo run -p decibell-app --features capture --bin decibell-capture -- --screen <name>
  --all-themes --out-dir <dir>`. Look at it live: `decibell --screen <name>`.
- **A self-test** is an `async fn(TestCx) -> anyhow::Result<()>` that drives the real app in its real window
  (`decibell --selftest <name>`): `cx.update(|window, cx| …)`, `cx.settle().await` (wait until drawing
  stops), `cx.frames()`, `cx.keystroke("f8")`, `cx.click(point)`, `cx.sleep(d).await`, `cx.log(…)`. Use it for
  what only the real frame loop can show: that a screen goes idle, how many frames an interaction costs.
  Assert frame counts with `Frames::since`. Logic that needs no window is a unit test or `#[gpui::test]`.
  See `crates/decibell-app/src/selftests.rs`.
- Self-tests open a real window on the session's display (GPUI cannot hide a window on Linux) and leave the
  theme and state as they found them. A capture never opens one.

## GPUI gotchas met so far

(`docs/gpui-notes.md` §12 has the survey's list; these were hit while building the skeleton.)

1. A path dependency under the workspace directory becomes a workspace member: `exclude = ["vendor"]` in
   `Cargo.toml` is what keeps GPUI's crates resolving `workspace = true` against Zed's manifest.
2. `BoxShadow::blur_radius` is the Gaussian's σ; GPUI's own `shadow_md()` presets copy CSS numbers and are
   therefore twice as blurry as Tailwind's. Use the theme's shadows.
3. `svg()` paints nothing without `.text_color(…)` of its own, and takes its colour when the parent renders:
   a parent's `.hover(|s| s.text_color(…))` does not reach it (hence `Icon::group_hover`).
4. The first key press after pointer input (and the first pointer event after a key) redraws the whole window
   once, uncached: GPUI tracks the input modality for focus-visible styling.
5. `HeadlessAppContext::update` does not flush effects; `update_window` does. A capture taken after a plain
   `update` shows the previous frame.
6. `Window::window_decorations()` is `Server` on Windows and in headless test windows whatever was asked
   for. Frame code asks `decibell_ui`'s `window_frame::decorations`, never the window.
7. Client-side decorations: the window's surface includes a transparent margin (`set_client_inset`), so
   `window.viewport_size()` is 20 px larger than the visible window on Linux; `window_min_size` and the
   compositor's geometry are the visible size. Open windows at `decibell_ui::outer_window_size(visible)`.
8. On Linux a window cannot be created hidden or unfocused (`WindowOptions::show` / `focus` are ignored).
9. Nothing in GPUI says "this frame reached the screen". `decibell_harness::perf` uses the first next-frame
   callback after a present (the compositor's frame callback on Wayland).
10. Monospace faces of one family share every metric GPUI exposes; only their outlines tell the weights
    apart (the font tests rasterise an `m`).
