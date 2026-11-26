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
  or `px(n)` where the TSX has an arbitrary `[n px]`. The exceptions are the renderer's own literals, each
  commented where it is used: the window's shadow (`window_frame.rs`), the close button's hover white
  (`hover:text-white`), the dialog backdrop's `rgba(0,0,0,0.65)` (`modal.rs`) and nocturne's select-menu sheen
  (`select.rs`, a globals.css component rule).
- Converting tokens costs nothing at paint: `Theme` holds `Hsla` / `BoxShadow` values made once per theme
  switch. Don't call `decibell_ui::hsla` / `box_shadows` in `render`, except for a palette that is not the
  active one (a theme preview).
- `Theme::set(cx, id)` switches and repaints every window; nothing else is needed in views.
- CSS → GPUI differences already handled in `decibell-ui`: a CSS blur radius is twice GPUI's `blur_radius`;
  CSS paints the first shadow layer on top, GPUI the last. Letter-spacing (`tracking_*` tokens) is drawn by
  `decibell-text` (`Label::tracking(em)`; the theme's body tracking is the default), not by GPUI's own text
  elements. Not handled anywhere: no percentage radii (use `theme.avatar_radius(size)`), a spread shadow keeps the
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
  There is no italic face: `Label::italic()` / an italic run draws a slanted twin of the upright face
  (`decibell-text`, GPUI patch 0010), as Chromium synthesises it.
- Adding a weight or family: `tools/gen-fonts.py` (`FAMILIES`), run `tools/gen-fonts.sh`, commit
  `assets/fonts/static/` and `crates/decibell-ui/src/fonts_generated.rs`. The unit tests and the `boot`
  self-test then check the new face resolves to itself.

## Icons

- `Icon::new(IconName::StageMic).size(px(18.)).color(theme.color_text_secondary)`. `IconName` is generated
  from the files under `assets/icons/` (`stage/mic-off.svg` → `StageMicOff`, root files unprefixed), so a
  missing icon is a compile error. The colour defaults to the inherited text colour; `.stroke_width(2.5)`
  redraws the glyph at another width in its 24-unit box (lucide-react's `strokeWidth`, the stage's `bold`);
  `.group_hover("name", colour)` follows a hovered ancestor that called `.group("name")` (an icon cannot
  inherit a hover colour: its colour is fixed when its parent renders).
- The set: the renderer's glyph components, extracted from the real TSX by
  `tools/electron-captures/icons/extract.sh` (`settings/` = settingsUi's stroke set, `settings-tab/`,
  `server-tab/`, `stage/`, `viewer/`, `picture/`, `transfers/`, `chat/`, `voice/`, `call/`, `devices/`),
  the kit's own glyphs at the root and in `status/` (copied from the components that draw them), lucide
  (`tools/fetch-assets.mjs`, driven by the renderer's lucide-react imports) and `window/`.
- Adding an icon: a glyph component the renderer exports or defines locally → add it to
  `tools/electron-captures/pages/icons-dump.tsx` and rerun `icons/extract.sh`; an inline `<svg>` inside a
  bigger component → copy it as a file next to the others (one colour: `currentColor`, the stroke width on
  the root `<svg>`). `decibell-ui`'s build script embeds every `.svg`; `icon::tests::every_icon_rasterises`
  checks each one draws through GPUI's own rasteriser. SVGs are drawn as one colour (alpha mask + tint).

## Components (`decibell-ui`)

Build screens from these; don't restyle a button by hand. Each is a builder like GPUI's own elements, reads
the theme, and has the renderer's hover / focus-visible (the theme's `shadow-ring`) / disabled states. Live
gallery: `decibell --screen kit-forms` (and `kit-buttons`, `kit-overlays`, `kit-dialogs`, `kit-settings`,
`kit-settings-rail`, `kit-identity`, `kit-scroll`); each has an Electron twin in `tools/electron-captures`.
Callbacks take an event so `cx.listener(|this, _: &ClickEvent, window, cx| …)` fits. Keyboard: every
control is a tab stop and Enter / Space activate it; `decibell_ui::init` binds Tab / Shift-Tab to
`FocusNext` / `FocusPrev`, which move focus only where a window's root element has
`decibell_ui::focus_navigation(…)` (GPUI binds no keys itself; a `Modal` keeps them inside).

- Scrolling: `ScrollArea::new(id).track_scroll(&handle).flex_1().child(content)` (Chromium's wheel / key
  animation and scrollbar; padding goes on the content; `.gutter_stable()`, `.without_keys()`,
  `.windowed(count, RowHeights::Uniform(px(44.)), |range, window, cx| rows)` for long lists);
  `ScrollHandle::new()` (`scroll_to`, `scroll_to_bottom`, `scroll_into_view(top, height, ScrollAlign)`,
  `is_at_bottom`, `shift`); `scroll::ScrollAnimator` (the pure model, for the message list's own element).
- Buttons: `Button::new(id, "Save changes").variant(ButtonVariant::Danger).size(ButtonSize::Sm).icon(…)
  .loading(saving).disabled(…).full_width().on_click(…)`; `IconButton::new(id, IconName::Close).small()`;
  `LinkButton::new(id, "Undo")` (`.recolor()`: the menu idiom); `Spinner::new().size(px(20.))`.
- Forms: `Switch::new(id, on).on_toggle(…)`; `Checkbox::new(id, on).label("…")`;
  `SegmentedControl::new(id, vec![Segment::new(v, "Low"), …], selected).on_change(…)`;
  `Slider::new(id, value, min, max).step(10.).fill().ticks([…]).on_change(…).on_commit(…)`;
  `Select::options(id, Some(value), vec![SelectOption::new(v, "Label").hint(…)])` or `Select::new(id, value,
  groups)` (`.size(SelectSize::Sm)`, `.variant(SelectVariant::Sunken)`, `.placeholder(…)`, `.icon(…)`,
  `.track(&SelectHandle::new(cx))` to open / inspect it from code); `Field::new("Label").hint(…)
  .error(…).child(control)` (`Field::plain` for dialogs); `TextField::new(id).focus(&handle).value(…)
  .placeholder(…).error(…)` — **a shell only** until `decibell-text`'s editor is passed to `.input(editor)`.
- Overlays: `Modal::new(id, open).on_close(|request: &CloseRequest, …| …).child(dialog_card(400., cx)…)`
  (render it open or not; content must `.occlude()`; refuse a close by not closing);
  `ConfirmModal::new(id, open, "Delete message", "Delete").message(…).tone(…).busy(…).on_confirm(…)
  .on_cancel(…)`; `ContextMenu::new(id, point).entry(MenuItem::new("Kick").icon(…).danger())
  .entry(MenuEntry::Separator).on_dismiss(…)` (`MenuEntry::Label`, `Custom`, `MenuItem::checked`,
  `expander`, `keep_open`); `Popover::new(id, point).child(…)`; `place_menu(…)` / `Placed` for your own
  layer; `Tooltip::new(id, "Audio", bounds).shown(hovered)` (the settings rail's label) and
  `title_tooltip("…")` for GPUI's `.tooltip(…)`; `Toasts::push(cx, Toast::error("…").body(…).action(…))`
  with one `ToastStack::new()` in the **root** view (not inside a cached view).
- Structure: `SettingsWindow::new(id, open, &scroll).nav_label(…).item(NavItem::new(id, icon, "Audio")
  .active(…).dot(dirty)).foot_item(…).title(…).context(…).on_close(…).child(tab)` (folds to the 64 px rail
  below 1024 px); `SaveBar::new(id, &pane_scroll, dirty, nudge).saving(…).on_reset(…).on_save(…)` (bump
  `nudge` on a refused leave: it shakes); `SectionLabel::new(…)`, `Heading::new(…)`, `divider(cx)`,
  `Badge::new(…).tone(BadgeTone::…)`, `Notice::error(…)`, `ErrorCard::new(id, …)`, `EmptyState::new(icon,
  title, body)`, `Banner::reconnecting()` / `Banner::notice(id, …)`.
- Identity: `LetterAvatar::new(uid, name, 32.)`, `Avatar::new(uid, name, 32.).image(decoded)`,
  `StatusDot::new(Presence::Away, ring).size(13.)`, `StatusPill::new(…)`, `BellMark::new(id).rise(true)`;
  `identity::person_slot(uid, name)` / `avatar_color` / `name_color` (the renderer's colour hash).
- Motion: `motion::transition(window, cx, id, target, Transition::TAILWIND)` (a CSS transition: frames only
  while it moves), `transition_flag`, `transition_from` (an entrance), `motion::mix` (colours as CSS
  interpolates them), `Keyframes`, `CubicBezier` (`EASE`, `TAILWIND`, `POP`…). One-shot entrances use GPUI's
  `with_animation` with these curves.
- Gotchas found building the kit: (1) GPUI's `opacity` is not a group composite: a disabled button's label
  is fainter than in Chromium; (2) no transforms on boxes: entrances keep their translate (as a relative
  offset) but drop the `scale(0.95)`; (3) no letter-spacing: `tracking-*` labels are tighter; (4) state held
  in an entity the owner made (not `window.use_keyed_state`) must notify the view it was drawn in, or a
  cached view replays its old paint (see `select.rs` `update_state`); (5) GPUI keeps *presenting* (not
  drawing) for 1 s after six inputs within 100 ms: time animations by drawn frames; (6) `capture` renders one
  frame on a clock that does not run: kit screens turn on reduced motion there so entrances show their end.

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
