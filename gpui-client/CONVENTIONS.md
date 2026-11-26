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
| `decibell-platform` | The desktop around the window (what Electron's main process did that needs the OS): one instance per profile, `decibell://` links and `--hotkey`, attention, taskbar progress, the quit guard, window state, sleep / lock. | gpui, model, ui, harness |
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
  .loading(saving).disabled(…).full_width().on_click(…)`; variants `Primary`, `Secondary`, `Ghost`, `Danger`,
  `DangerSoft`, `DangerOutline`, `Soft`, `Outline` (`rounded-sm border-border text-secondary`, border
  `accent/40` on hover: Record again, Test mic) and `QuietOutline` (the same with `hover:bg-surface-hover`:
  E2eeSection's buttons); sizes `Lg` (`px-5 py-2.5`, 13 px: Log Out), `Md`, `Sm`, `Xs` (`px-2 py-0.5`,
  12 px); `.rounded_sm()` where a variant's `rounded-md` is `rounded-sm` in the TSX (a device's Sign out);
  `.probe(key)` for the self-tests (`decibell_harness::probes`). `button.rs`'s table maps each to its
  classes. `IconButton::new(id, IconName::Close).small()`; `LinkButton::new(id, "Undo")` (`.recolor()`: the
  menu idiom); `Spinner::new().size(px(20.))`. A hover colour that transitions on an element of your own:
  `Hover::new(window, cx, id, Some(Transition::TAILWIND))` (`progress`, `listener()`).
- Forms: `Switch::new(id, on).on_toggle(…)`; `Checkbox::new(id, on).label("…")`;
  `SegmentedControl::new(id, vec![Segment::new(v, "Low"), …], selected).on_change(…)`;
  `Slider::new(id, value, min, max).step(10.).fill().ticks([…]).on_change(…).on_commit(…)`;
  `Select::options(id, Some(value), vec![SelectOption::new(v, "Label").hint(…)])` or `Select::new(id, value,
  groups)` (`.size(SelectSize::Sm)`, `.variant(SelectVariant::Sunken)`, `.placeholder(…)`, `.icon(…)`,
  `.track(&SelectHandle::new(cx))` to open / inspect it from code); `Field::new("Label").hint(…)
  .error(…).child(control)` (`Field::plain` for dialogs); `TextField::new(id).editor(&state).error(…)
  .leading_icon(…).disabled(…).variant(TextFieldVariant::Dialog)` (the form dialogs' `bg-bg-dark rounded-sm
  py-2` box) or `TextFieldVariant::Compact` (Network's `w-28 bg-bg-mid px-2.5 py-1.5 text-[12px]`), with
  `state: Entity<decibell_text::input::InputState>` made once by the view
  (`InputState::single_line(window, cx).placeholder(…).max_bytes(64)`, `password`, `multi_line`, `composer`;
  read `value()`, subscribe to `InputEvent::{Change, Submit, Escape}`); a multi-line state grows the field to
  `.max_height(px)` and then scrolls. `crates/decibell-text/README.md` "Inputs" has the whole API; live:
  `decibell --screen inputs`.
- Overlays: `Modal::new(id, open).on_close(|request: &CloseRequest, …| …).child(dialog_card(400., cx)…)`
  (render it open or not; content must `.occlude()`; refuse a close by not closing; `.on_enter(…)`: Enter
  anywhere in it before the focused control, as a document listener; `.within(region, radius)`: dim and
  centre in a region, `settings::dialog_region` for a dialog the renderer draws inside a settings window);
  `ConfirmModal::new(id, open, "Delete message", "Delete").message(…).tone(…).busy(…).on_confirm(…)
  .on_cancel(…)` (Enter confirms as it opens and with Cancel focused; Space activates the focused button;
  `kit-confirm`); `ContextMenu::new(id, point).entry(MenuItem::new("Kick").icon(…).danger())
  .entry(MenuEntry::Separator).on_dismiss(…)` (`MenuEntry::Label`, `Custom`, `MenuItem::checked`,
  `expander`, `keep_open`); `Popover::new(id, point).child(…)`; `place_menu(…)` / `Placed` for your own
  layer; `Tooltip::new(id, "Audio", bounds).shown(hovered)` (the settings rail's label) and
  `title_tooltip("…")` for GPUI's `.tooltip(…)`; `Toasts::push(cx, Toast::error("…").body(…).action(…))`
  with one `ToastStack::new()` in the **root** view (not inside a cached view).
- Structure: `SettingsWindow::new(id, open, &scroll).nav_label(…).item(NavItem::new(id, icon, "Audio")
  .active(…).dot(dirty).probe(key)).foot_item(…).title(…).context(…).on_close(…).child(tab)` (folds to the 64 px rail
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

## Shell (`decibell-shell`, `decibell-common`)

The app's content under the title bar is `decibell_shell::Shell`: nothing until the core has read the
configuration, then the sign-in page (`AuthView`) or the main layout (`MainView`: banner, `ServerBar`, the
rounded workspace with the sidebar group — `ChannelSidebar` or `ConversationSidebar` with the floating
`UserPanel` — and the pane for the active view). Every part is its own cached view. Feature crates plug in
through `decibell_shell`, once, from their `init(cx)` (called by `decibell-app`'s boot before the window
opens):

- **A pane**: `register_pane(cx, Slot::Chat, |window, cx| cx.new(|cx| ChatPanel::new(window, cx)).into())`.
  Slots: `Home` (`decibell-home`'s Friends page), `Dm`, `Chat` (under the channel header, left of the right
  panel), `RightPanel` (default: `MembersList` while the members panel is on; a registered one decides
  itself, e.g. search results or `cx.new(MembersList::new)`), `HeaderSearch` (244 px at the header's end),
  `Voice`, `Browse` (the whole workspace, no sidebar), `DmList` (default: the shell's rows from `dm.list`),
  `UnreadDms` (default: the shell's tiles), `DmCallStage` (built by the DM pane: `build_slot`). A pane is built when its place first shows, dropped when it
  goes (React's mount / unmount), drawn `.cached(…)`: it must watch what it reads. Unregistered places show
  `Placeholder` (the slot's name).
- **An overlay** (a modal, popup, menu over the main layout): `register_overlay(cx, |window, cx| …)`; it
  lives while signed in and decides from the model when it is open (`model.overlays` is where the shell's
  openers put requests: `decibell_common::open_user_menu`, `open_profile_beside`, `Overlays::open_modal`).
- **A context-menu entry** on the channel list: `register_channel_menu(cx, |target: &ChannelMenuTarget, cx|
  vec![MenuEntry::from(MenuItem::new("Mark as read").on_click(…))])`, appended after the shell's own.
- **A dialog the shell asks for**: `ShellRequests::entity(cx)` (observe it): `create_channel` (the channel
  list's create actions), `leave` (the shell draws that confirmation itself).
- **People**: draw avatars with `decibell_common::UserAvatar` (picture or letter, the status dot, and while
  drawn the presence interest and the picture fetch), names with `display_name(server, uid, &fallback, cx)` /
  `user_name`, custom statuses with `StatusMessageText`. Never draw a uid; "Unknown user" is the fallback.
- **Keep a cached view current** with `decibell_common::Watch`, listing every store `render` reads, once,
  in `new`: `.store(&e, cx)` (every notify), `.store_if(&e, cx, |this, e, cx| changed)` (compare the part
  shown, e.g. the navigation's view), `.event(&e, cx, |this, ev| this.shows(ev.key))` (keyed events),
  `.people(cx, |this, uid| this.shows(uid))` (names, pictures, statuses, messages). A community's own
  entities (channels, roster) change with the active community: re-subscribe when it changes (see
  `ChannelSidebar::follow`).
- **A cached view is laid out at the size its parent gives it** (GPUI makes it a leaf node from the
  `.cached(style)` and lays the view out as a root inside): give it an explicit size (`size_full` in a sized
  box, a fixed width), and make the view's own root `size_full` / `w_full`, never `flex_1` (as a root it
  would shrink to its content). A view that must size itself (a registered `RightPanel`) is drawn uncached.
- **GPUI redraws the whole window, every view, when the window's focus changes** (measured):
  idle measurements count those; the shell's checks retry a disturbed window (`shell-idle`).
- **Colours in a render that also needs `&mut cx`**: `crate::ink::chrome(cx)` / `base(cx)` (a `Copy`
  snapshot with `Tokens`' field names; shadows are read from `cx.theme()` where drawn).
- **Self-tests that need a signed-in app** run it as a child: `decibell --check <name> --login perf1`
  (`decibell_shell::checks()`; a binary with `dev-local`), with its own profile and `DECIBELL_TEST_PROBES=1`,
  which makes `decibell_harness::probes::probe(key)` (also `decibell_shell::probes`) record where elements
  are so the check can click them (the kit's `Button` and `NavItem` take `.probe(key)`).

## Chat (`decibell-chat`)

The message list and the pane around it. `crates/decibell-chat/src/list/core.rs` (module doc) is the design;
in short:

- **`MessageList`** is one view whose `render` returns one custom element. Rows are not elements: each row is a
  `RowLayout` (`row.rs`) built once from its message, facts and the theme (`ChatStyle`, resolved once per theme /
  text size / window scale), holding its exact height (Blink's, in 1/64 px), the body's `RichLayout`, the
  header's label layouts and the boxes of its slots; a frame paints the rows that meet the viewport from those
  with `paint_quad` / `Layout::paint` / `RichLayout::paint`. A frame in which only the offset changes builds
  nothing, lays nothing out and shapes nothing.
- **Coordinates and the anchor.** Every row has a top in content coordinates (an f64 that is never
  renormalised) and the scroll position is a number in the same space; a row is drawn at
  `round(top × scale) − round(position × scale)`. Whenever rows or heights change, tops are recomputed outward
  from the first row in the viewport, whose own top is kept: a page spliced in above gets tops below the old
  minimum and changes neither the anchor's top nor the position, so nothing on screen can move. Don't add a
  "scroll correction" anywhere: change what `ListCore::apply` / `heights_changed` anchor on instead.
- **Measuring.** Rows in and around the viewport are measured in `prepaint` before anything is painted; the rest
  of the slice's window in slices of ≤ 1.5 ms on idle turns of the main thread, nearest the viewport first, an
  estimate (the mean of the measured rows) standing in meanwhile. A late exact height moves only rows on the far
  side of it from the anchor.
- **The model does the rest**: rows come from the slice's change log (`changes_since`), pages from
  `ListSource::load_older` / `load_newer` when the position is within 800 px of an end, trims from
  `MessageSlice::trim_head` / `trim_tail` with the renderer's pixel rule (the list passes `keep`, computed from
  pixel distances; nothing trims by row count alone).
- **Open the chat pane on a channel**: `cx.new(|cx| ChatPane::new(server, channel, window, cx))`, or
  `ChatPane::for_active_channel(window, cx)` for the shell's slot (it follows the navigation):
  `decibell_shell::register_pane(cx, Slot::Chat, |window, cx| cx.new(|cx| decibell_chat::ChatPane::for_active_channel(window, cx)).into())`.
  A DM pane gives `MessageList::new` its own `ListSource` (a slice plus `load_older`, `jump_to_message`, `edit`,
  `delete`, …: `source.rs`); `ChannelSource` is the channel one.
- **Slots other crates fill** (`media.rs`; all optional, the list reserves the same box and draws a neutral
  placeholder until then). Register once at start-up: `decibell_chat::slots(cx).attachments = Some(Rc::new(…))`.
  - `attachments`, `inflight`, `embeds`: a `RowSlot`: `height(&Message, server, SlotSpace, &App) -> f32` (final
    for those inputs, its own `mt-1` included; 0 = nothing) and `element(&Arc<Message>, server, SlotSpace,
    window, cx) -> AnyElement`, laid out at the reserved box. `SlotSpace` carries the content column's width and
    the chat view's size (`chatViewSize`). When an answer may have changed (a link preview resolved, an upload
    ended) call `decibell_chat::invalidate_slots(cx)`: the rows are measured again around the anchor.
  - `editor`: an `EditorSlot` (`height(&Message, width, &App)`, `element(&Arc<Message>, width, submit, cancel,
    window, cx)`): the inline editor of `MessageList::start_edit`. Until `decibell-text`'s `TextArea` fills it the
    list draws the field as a still and Escape cancels.
  - `composer`: `Rc<dyn Fn(ServerId, ChannelId, &mut Window, &mut App) -> AnyView>`, drawn under the list by
    `ChatPane`. The list tells the pane what the composer needs through `ListEvent::Reply(id)`; the composer
    shows the rail with `MessageList::set_reply_target`.
  - `ListEvent::OpenMedia(message id)` is the click on a media placeholder (the image viewer's), `OpenLink(url)`
    a link (`ChatPane` hands it to `cx.open_url`; invite links open the join confirmation themselves).
  - `ListEvent::AtBottom(bool)`: the list rests at its **live bottom** (within `AT_BOTTOM_PX` of the end, not
    windowed), emitted after a drawn frame only when it changed (the first frame always says; a list hidden by
    its owner is asked to say again with `retell_bottom`). The DM pane's "being read" rule reads it.
- **Options**: `MessageList::new(source, cx).with_right_padding(16.)` keeps the rows 16 px from the scrollbar
  (the DM list's `className="pr-4"`; the scrollbar stays at the edge).
- **"Today" moves on at midnight**: a list keeps one timer, for the next local midnight
  (`regional::next_local_midnight`), that rebuilds its rows' stamps against the new day and re-arms itself (an
  early wake on a DST day only re-arms); `set_clock` injects the wall clock in tests. The renderer's bubbles are
  memoised and keep yesterday's "today" until something re-renders them; ours relabel.
- **CSS that the row layout reproduces by hand** (found by comparing every row's `offsetTop` with the Electron
  client, `tools/electron-captures/chat-rows.mjs`): a block (code, display math) first in a body collapses its
  top margin with the body's `mt-0.5`; two blocks in a row share one margin; a block last in a body collapses
  its bottom margin with the `mt-1` of the slot under it; a code block's horizontal bar adds 10 px to its
  height and a collapsed one's vertical bar takes 10 px of its width.
- Screens `chat-fixture` / `chat-features`, self-tests `chat-*`, the benchmark `decibell --bench-scroll <s>`
  (and `cargo run --release -p decibell-chat --features fixtures --example chat_bench`, which also counts
  allocations per frame), the fixtures in `fixture.rs` (the 10,000-message channel of `tools/dev/fixture.py`,
  generated bit for bit, and a hand-written one). Against the local stack: the self-test `chat-live` (runs
  `decibell --check chat-live --login perf2`: `#general` paged back 2,000 messages by scrolling, 0 px per page,
  a jump, the present), the check `chat-copy` (`DECIBELL_COPY=<channel>:<from>[@offset]:<to>[@offset]`, compared
  with `DECIBELL_COPY_EXPECT`, a file of the Electron client's clipboard for the same selection) and the
  `scroll` verb of the dev control pipe (`tools/perf/scenarios.sh gpui scroll`). `chat-hover` (fixture): a hover
  action's tooltip after its delay, no menu on a right click, the last row's entrance ending in idle.
- **Copy** is what the Electron client's clipboard holds for the same selection (`RowLayout::copy_text`,
  `CopyText`, `decibell_richtext::plain::as_copied_range`; pinned by unit tests to strings read back from its
  clipboard): `user-select: none` parts (avatar, quoted reply, day divider, a grouped row's hover time) copy
  nothing; "(edited)" does (the renderer's `.select-text *` rule wins); Twemoji emoji copy nothing (inline SVG);
  a code block copies its label as written in the DOM; attachments copy their `alt` / card text. A slot that
  draws text (link previews, invite cards) adds nothing yet: the slot API has no copy hook.
- **No message menu**: a right click opens the sender's user menu on the name or avatar, nothing elsewhere (the
  renderer's only menu on a message). The hover actions and the code block's copy button show their `title=` as
  a GPUI tooltip after 500 ms; the last row fades up (`isLast`, 300 ms) and the jump pill fades in, both drawn
  only while they move and skipped under reduced motion.
- **Slot API changes for the composer**: `ComposerBuilder` is `Fn(ServerId, ChannelId, &Entity<MessageList>, &mut
  Window, &mut App) -> ComposerParts { view, type_to_focus }` (the composer follows the list's `ListEvent::Reply`
  and calls `start_edit`; the pane's key listener hands keys that reach it from the list to `type_to_focus`), and
  `EditorSlot::height(&Message, width, scale, &App)`. Keys that reach no pane (the app's `Root` holds the
  focus when nothing else does) go to the composer on screen through the root's own bubble-phase listener
  (`decibell_composer::type_to_focus`, called from `decibell-app/src/root.rs`).

## Composer (`decibell-composer`)

What ChatPanel.tsx / DmChatPanel.tsx draw under the list, and the inline editor. `decibell_composer::init(cx)`
(after the chat crate's, in `registry::init_features`) fills the chat crate's `composer` and `editor` slots.

- **One `Composer` view per chat pane**, generic over where it sends: `Composer::new(Rc<dyn ComposerTarget>,
  Some(list), window, cx)`. `ChannelTarget { server, channel }` is the channel pane's (registered by `init`);
  **the DM pane** hosts the same view on a `DmTarget { peer }` under its list (drawn uncached: the card sizes
  itself): `decibell_composer::dm_composer`, which the app joins into `decibell_home::dm::set_dm_composer`,
  returns it with `Composer::type_to_focus_hook(&composer)` for the pane's key listener. `ComposerTarget` is the
  seam (send / send_gif / set_draft / latest_own_editable / attach / gate / send_error / what to observe); the
  `Variant` picks the renderer's differences (the DM card has no attach button and no slowmode, `rounded-sm`
  buttons, the inline send error above the card).
- **Drafts** live in the model (`model.chat.drafts`): the composer loads its key's draft when it is built (one
  composer per channel visit) and writes every change; a refused send's `DraftsEvent::Restored` puts the text back.
- **Sending** goes through the model's actions (`send_message` / `send_gif` / `send_dm`): the box clears and the
  reply ends only on `SendOutcome::Sent`; a slowmode hold keeps the text and shakes the pill (`SlowmodeEvent::Nudged`).
- **Popups anchor in the same frame**: `panel::Above` fills the box it is put in (position the box `relative`) and
  places its child above it, left or right edge aligned, laid out at the child's own size. Boxes remembered from
  the last frame (`track_bounds`) are only for hit tests and self-tests; an absolute child without insets sits at
  its static position, not on its parent: give such canvases `inset_0()`.
- **Picker memory**: the emoji dataset is static data (`build.rs` from `assets/emoji-data/native.json`); the
  picker rasterises Twemoji into its own two-generation cache (~4 MB cap) and gives every image back
  (`drop_image`) when it closes; GIF previews are fetched only for tiles near the viewport and their decoded frames
  and atlas tiles are released on close. Close through `Composer::close_picker(window, cx)` (it needs the window)
  and never from inside a picker update (`Picker::close` defers it).
- **Outside a chat pane**: `dm_composer` (the DM pane's composer, joined by the app), `status_emoji_picker` (the
  custom status dialog's picker: `Picker::emoji_only`, no GIFs tab, a pick handed to the caller, its images
  released when the caller drops it), `type_to_focus` ("type anywhere", the renderer's rule: with no text
  field focused (`decibell_text::input::text_field_focused`), printable keys and ArrowUp go to the composer
  mounted last while it lives and its host draws it (`ComposerTarget::hidden`: the DM call's theater); not with
  Ctrl / Alt / Super, a modal, or the notice instead of the card; called by the app's root view in the bubble
  phase, after every element on the focused path). **A key the focused hotkey fallback takes is the
  hotkey's** (`Hotkeys::would_take`: it completes an armed binding it is part of, e.g. a bare-key push to talk)
  and is not typed; every other printable key goes to the composer (`docs/electron-quirks.md` #17; self-test
  `join-type-to-focus-hotkeys`).
- **Timers**: the slowmode countdown ticks on the wait's whole-second boundaries only while a window runs; the
  timed-out notice once a second while timed out; the GIF debounce only while a query waits; the drop pulse only
  while a file drag is over the window. Nothing at rest (`composer-idle`: 0 frames blurred).
- **GIF key**: `GIF_API_KEY` (+ `GIF_API_PROVIDER`), else `resources/gifs.json` beside the executable (what a
  packaged install ships), else, in a dev checkout, `electron-client/resources/gifs.json` found above the
  executable (`gifs::key_file_candidates`). Never printed.
- Screens `composer-*`, `emoji-picker*`, `gif-picker*`, `code-panel`, `math-panel`, `inline-editor` (Electron twins
  in `tools/electron-captures/pages/`); self-tests of the same names run against the local stack as `perf2` in a
  child app (`--check`), posting only in `#off-topic` / `#slow-lane` and deleting what they post (waiting for the
  server's confirmation, not the optimistic removal).

## Server and channel settings (`decibell-server-settings`, `decibell-picture-editor`)

- **Mount points.** `decibell_server_settings::init(cx)` registers two shell overlays
  (`host::ServerSettingsHost`, `host::ChannelSettingsHost`). They hold nothing while their modal is closed;
  opening (`overlays.open_modal(modal::SERVER_SETTINGS)`, `overlays.open_channel_settings(id)`) builds the
  window for `nav.active_server` (and the channel), closing lets it fade 300 ms and then drops it with every
  tab, search, subscription and picture it held. Tabs are entities built when first shown and dropped on a
  switch (`ServerSettingsWindow::show`); nothing polls, timers exist only while they have a reason (a
  250 ms search debounce, an 8 s "no answer", a 5 s echo wait, a role assignment's expiry).
- **Requests.** The model has no actions for administration, so every call into `decibell_core::api` is one
  `requests::Request` sent through `requests::send` / `fire`. A `Requester` installed with
  `requests::install` sees each request first and may answer it (`fixture::install`: the screens' community
  server, answering through the model's real router after the call returns) or let it through (the
  self-tests' recorder). Answers no store keeps arrive on `Model::answers`; a view subscribes while open.
- **Draft → echo.** An edit lives in the tab's draft (`Option<…>`: `None` follows the server) and the shown
  value is the draft's, else the store's. Save starts a `save_echo::EchoSave` with what was sent and sends
  it; the draft stays ("Saving…") until a store change makes the stored value equal what was sent
  (`settle`), a refusal on `model.answers` (`stop`) or 5 s ends it and keeps the draft. Never clear a draft
  when a request goes out. Channel settings keep one draft for Overview and Retention plus a permission
  draft (staged overwrites in the order they'll be sent, the category switch); Save sends rename → update →
  overwrites → category switch in order (`send_in_order`), and the draft drops rows as the server's pushes
  show them. Text fields clamp to the server's byte caps as typed (`InputState::max_bytes`).
- **Leave guard.** The window owns a shared `Nudge` (`Rc<Cell<u32>>`) handed to every tab. Esc, ✕, a backdrop
  click or a tab switch while the open tab is dirty bump it instead of leaving; `SaveBar::nudge(n)` shakes on
  every new value. The Roles tab does the same on a role switch, channel settings on close. Edits are never
  dropped silently.
- **Lists.** Long lists (members, a role's holders, bans, the audit log, pickers) draw only the rows that meet
  the pane's viewport (`rows::PaneRows`: uniform or measured heights, measured once per width) inside the one
  settings scroll; toolbars stick with `rows::StickyTop`. Member lists come from the server's search
  (`member_search::MemberSearch`, patched by roster deltas), never from the roster store.
- **Over a modal.** GPUI paints deferred layers by priority alone, so a popover or select opened inside a
  modal must be built `.over_modal()` (`Popover`, `Select`), or it is drawn under the modal.
- **Mixed-style sentences** (audit entries, counts with inline dots / avatars) are one paragraph
  (`sentence::Sentence` over `decibell_text`), with inline boxes filled by elements; an inline box only as
  tall as the part above the baseline keeps the CSS line height.
- **Picture editor** (`decibell-picture-editor`, no dependency on the settings crates): open it with the
  source and the shape, draw it, and answer its Save with the upload's result:

  ```rust
  let editor = cx.new(|cx| PictureEditor::new(
      PictureSource::Path(path),                 // or PictureSource::Bytes { bytes, name }
      EditorOptions::avatar(uid, name, Presence::Online), // 256 × 256, round; or ::server(name, members): 880 × 302
      window, cx));
  cx.subscribe(&editor, |this, editor, event: &PictureEditorEvent, cx| match event {
      PictureEditorEvent::Save(jpeg) => { /* upload jpeg (≤ 1000 KiB JPEG at the output size), then: */
          editor.update(cx, |e, cx| e.save_done(Ok(()) /* or Err(message.into()) */, cx)) }
      PictureEditorEvent::Closed { saved } => { this.editor = None; cx.notify() } // saved: the bytes, if saved
  }).detach();
  // render: .children(self.editor.clone())  — it is a modal; it fades out, then emits Closed and drops its textures
  ```
  `EditorOptions { shape, title, output, previews }` can be built by hand (`Previews::Custom` draws your own
  previews from the live crop). EXIF orientation is applied; HEIC and files over 80 MP are refused with the
  renderer's messages.

## Regional formats (`decibell_model::regional`)

Owner instruction (2026-11-26): the UI's language stays English; **dates, times of day and numbers follow
the system's regional settings** (Linux / macOS: the locale `LC_ALL`, else `LC_TIME` / `LC_NUMERIC`, else
`LANG` names, read through `newlocale` + `nl_langinfo_l`; Windows: the user's format settings with their
overrides). The Electron client formats by its UI language instead (`docs/electron-quirks.md` #11).

- **Never format a date, a time of day or a count by hand**: `Regional::get()` (read once, cached; an atomic
  load per call). The renderer's helpers map one to one: `formatTimestamp` → `message_stamp(secs, today,
  pref)`; `formatClock` / `formatClockCompact` → `clock` / `clock_compact` (or `time(h, m, pref)`);
  `formatFullDateTime` → `full_date_time`; `formatDateTime` → `date_time`; `Intl.DateTimeFormat()` /
  `toLocaleDateString()` → `short_date(Date)`; `{ day, month: "short", year }` → `medium_date`; the day divider
  → `long_date`; `{ month, day }` → `month_day`; the date picker → `month_year`, `first_weekday()`,
  `weekday_narrow`; `n.toLocaleString()` → `group(n)` / `group_signed`; `formatAgo` → `relative(then, now)`. The
  module doc has the whole table.
- Unix seconds → the local wall clock only through `regional::local(secs)` (`chrono::Local`: the system zone,
  DST); a message's day as yyyymmdd through `chat::time::local_day`.
- A time of day takes the user's setting: `model.settings.read(cx).time_format()` (`Auto` = the system's 12 /
  24 hours). A view that bakes a time into a layout watches the settings store and rebuilds (the message list
  does).
- Words are the app's (English): `weekday_name`, `month_name`, `MONTHS`, "Yesterday, at", AM / PM. Compose
  phrases around the module's pieces (`format!("Until {}", regional.date_time(…))`), never around digits.
  Durations are not dates (call timers, the DM list's "5m", slowmode countdowns): write them as the renderer
  does. Decimal measurements (sizes "18.4 MB", dB) still follow the renderer's `toFixed` ("."); `decimal(f, n)`
  exists for when the owner wants them regional.
- **Tests and captures**: unit tests use explicit instances (`Regional::en_us()`, `Regional::neutral()`,
  `Regional::from_posix(Posix { … })`, `from_windows(…)`), never the global. Anything compared with Electron
  pins en-US: `let before = Regional::pin(Some(Regional::en_us()));` … `Regional::pin(before);` (the capture
  tool for its whole run, `chat-rows` for its own); `tools/electron-captures/run.sh` runs Electron with `LC_ALL=en_US.UTF-8` (the renderer's
  `Intl` takes ICU's locale from LC_ALL / LC_MESSAGES / LANG, not from `--lang`).
  Views keep what they built: pin before they build.
- The check for leftovers in a crate (test code aside): `rg -n '\{:0?2\}|"(AM|PM)"|January|"Jan"|"Monday"|"Sunday"|chrono::Local|Local\.timestamp|localtime|\b(count|len\(\)|total|n)\.to_string\(\)' crates/<crate>/src`.
## Settings (`decibell-settings`)

The Settings window (`SettingsModal.tsx`), its nine tabs, the rename / change-password dialogs and the
E2EE passphrase dialog. `decibell_settings::init(cx)` registers three overlays (the window, then the two
model-driven dialogs, so they draw over it); `SettingsHooks` is what the app hands in (its version, the
updater's buttons, the out-of-voice `MicTest`) and the seams other features fill (`PictureRequests`: the
avatar editor subscribes and opens over Settings).

- **The window** is open while `model.overlays.active_modal == modal::SETTINGS`, on the tab
  `model.nav.settings_tab` names (unknown → Account; Devices only while `auth.devices`). Open it from
  anywhere with `nav.set_settings_tab("devices")` + `overlays.open_modal(modal::SETTINGS)`.
- **A tab is its own view**, built when it shows and dropped when it goes (and 300 ms after the window
  closes: the fade-out), so what a tab holds lives exactly as long as React's mount: the Devices list load
  (`actions::devices::watch`), the mic meter's hold on the input-level reporting (`InputLevel::retain_meter`
  while the threshold bar shows), the keybind recorder's pause. It renders the kit's `SettingsPane` (title,
  ✕, the scrolling body) and is drawn cached at the pane's size through `SettingsWindow::pane`: a change
  in one tab redraws that tab only. Each lists what it reads in a `decibell_common::Watch`.
- **Saves follow the renderer**: everything applies at once (toggles, selects, sliders, presets: the model's
  `Settings::change` saves 250 ms later; the account's own settings through their actions). The one draft
  is Network's typed custom speed (applied on Enter or blur): while it is unapplied the tab shows the
  `SaveBar` and leaving (Esc, ✕, the backdrop, another tab) shakes it (`TabView::dirty` / `nudge`). A new
  drafted field: make the tab's `dirty()` say so and give it a `SaveBar`; the window does the rest.
- **Dates, times of day and counts** come from `decibell_model::regional` (Devices' "Last active" is
  `relative`, "Signed in" `medium_date`, counts `group`; Appearance's sample is `time(14, 32, pref)`).
  Automatic time format means the system's regional setting (the renderer said "system language"), and
  the Appearance tab says so with the system's real time.
- **Text fields** are `decibell_text`'s editor (`InputState`, passwords `InputState::password`) in the
  kit's `TextField`: `TextFieldVariant::Dialog` in the rename, change-password and passphrase dialogs
  (`dialogs::field`), `Compact` for Network's custom speed (`accept(numeric)`, as `type="number"`). Enter
  in a dialog's input submits (`dialogs::submit_on_enter`), Enter on a focused button presses it; the
  rename field lower-cases as typed and holds 32 characters. Network's box applies on Enter and on blur
  (`InputEvent::Blur`: a press elsewhere, Tab, the window going to the background), so a draft exists
  only while it has the focus: Esc then shakes the save bar. Rename and change-password dim the Settings
  window only, as the renderer's do (`DialogParts::in_settings` → `Modal::within`;
  `docs/electron-quirks.md` #12).
- **Keybinds** record with the model's `Recorder` on physical codes (`KeyDownEvent::code`,
  `ModifiersChangedEvent::key` for a modifier alone and its side): the German key labelled Z records
  `KeyY`. The app's root feeds the focused fallback (`decibell-app/src/hotkeys.rs`, self-test
  `hotkeys-focused`).
- **Log Out** is `actions::auth::log_out` (hangs up a call first, then signs out); the attachment speed
  caps reach the attachment service through the model (`CoreAttachmentService::set_limits`, at its
  creation and on every settings change).
- **Screens** (`--screen settings-…`, `passphrase-*`, `rename-modal`, `password-modal`; `settings-narrow` at
  `--size 900x800`) are the signed-in shell from the shell's fixture plus `decibell_settings::fixture`
  (devices, E2EE states, keybinds with the portal's status, audio devices, codecs, a fixed clock).
  `decibell-settings-fixture <dir>` exports the same state for the Electron pages
  (`tools/electron-captures/pages/settings-*.tsx`, `_settings_window.tsx`).
- **Self-tests** (`settings-*`) run the app as a child against the local stack, as the shell's do. The
  account flows (password, rename, E2EE) **register a throwaway account per run** (`t<random>`) and refuse
  unless the build honours `DECIBELL_CENTRAL` (debug, or the app's `dev-local`, which turns on this crate's
  `dev-local`) and it is loopback; perf1's checks put back what they change and log out (forgetting their
  device). `settings-keybinds-record` runs its child under XWayland: the X11 listener grabs exactly the bound
  combos, where the Wayland portal would open the desktop's confirmation dialog. (Self-tests and screens
  share one namespace: the self-tests that would take a screen's name carry a suffix,
  `settings-devices-signout`, `settings-keybinds-record`, ….) The checks type into the fields
  (`TestCx::type_text`, `keystroke("tab" | "enter" | "ctrl-a")`), click by probes (the user panel's gear
  `user-panel-settings`, the tabs `settings-tab:<id>`, the views' own) and record keybinds from events
  built as xkb's `de` and `tr` keymaps produce them (`KeyCode::Evdev` + what the layout types).

## Home / DMs / people (`decibell-home`)

Direct messages, the Friends page and rail, the profile popout and screen, the custom status dialog, and
the user menu. `init(cx)` fills `Slot::Home` (`FriendsPage`) and `Slot::Dm` (`DmPane`: one `Conversation`
per peer, the `FriendsRail` beside it while `dm_friends_panel` is on) and registers four overlays
(`ProfilePopup` on `overlays.profile_popup`, `ProfileModal` on `modal::USER_PROFILE`, `CustomStatusModal` on
`modal::CUSTOM_STATUS`, `UserMenu` on `overlays.context_menu`). The DM rows and unread tiles stay the
shell's (`DmRows`, `UnreadDmTiles`).

- **A conversation** (`dm::Conversation`) draws `MessageList` over `dm::DmSource` (a `ListSource` on
  `model.dm`; the list with `with_right_padding(16.)`, the renderer's `pr-4`), opens its newest page on mount
  (`actions::dm::open_history`), keeps `DmList::set_displayed` and follows `DmEvent::Resync`.
- **A DM is marked read only while it is being read** (owner, 2026-11-26; Electron differs:
  `docs/electron-quirks.md` #16): the conversation is the open view with its list drawn (not loading, not
  hidden by the call's theater), the window is focused (`decibell_harness::focus::window_focused`: active and
  visible, so never while minimised) **and** the list rests at its live bottom (`ListEvent::AtBottom(true)`:
  within 4 px of the end with nothing newer outside the loaded window; emitted after a drawn frame only when
  it changes, so measuring, pages and trims don't flap it; a scroll animation says `true` once it arrives; a
  list hidden and shown again is asked to say anew, `retell_bottom`). While all three hold the view keeps
  `DmList::set_reading(peer, true)` (a DM arriving for it isn't counted) and reads every new row
  (`actions::dm::mark_read`); otherwise a DM stays unread here and, through read sync, on the account's other
  devices until the three hold again (focus back, scrolled or jumped to the bottom, the conversation opened),
  and is marked then. Our own messages are never unread (any device of ours: `from_self`); sending neither
  waits for nor triggers a mark. Events only (activation, visibility, the list's event), no timer. Self-tests
  hold the window "active" with `decibell_harness::focus::hold_window_focus` (other windows share the desktop;
  visibility stays real). `decibell-headless` reads what it `dm-open`s (no window).
- Encryption: the header chip and the notice
  read `model.live.e2ee` and the peer's answer from `e2ee::PeerInfos` (one request per peer per (our status,
  their change marker); views call `PeerInfos::want(uid)` from observers, never `render`).
- **The DM composer slot**: `decibell_home::dm::set_dm_composer(cx, |peer, list: Entity<MessageList>,
  window, cx| -> ComposerParts)`, once at start-up: the app's `registry::join_features` hands in
  `decibell_composer::dm_composer` (the crates don't depend on each other; the capture tool calls
  `join_features` too). The view is drawn under the conversation, uncached (it sizes itself), with the inline
  send error above its card; it follows `list`'s `ListEvent::Reply`, starts edits with `list.start_edit`,
  sends with `actions::dm::send_dm` (its send button disabled while `DmList::sending(peer)`), keeps the draft
  in `chat.drafts` (`DraftKey::Dm`), and the conversation's key listener hands it the keys that reach it from
  the list (`type_to_focus`). With none registered nothing is drawn under the list.
- **The call stage slot**: `decibell_shell::register_pane(cx, Slot::DmCallStage, |window, cx| view)`. The
  conversation builds it (`decibell_shell::build_slot`) while `calls.status` is outgoing, connecting or active
  with its peer, draws it above the list **uncached** (size it yourself: `call_stage_voice_height` /
  `_stream_height`, or the panel in theater), and drops it when the call ends; in theater (`calls.theater`)
  the list and composer are hidden and the header shows "Show chat".
- **User menu sections**: `decibell_home::menu::register_user_menu(cx, UserMenuExtension { order, watch,
  build })`. `build(&UserMenuTarget { uid, server, name, display_name, own }, window, cx) -> Vec<MenuSection>`;
  a `MenuSection { above: Above::Rule | Inset | None, element }` (the frame draws `above` only when a section
  precedes it). `watch(cx: &mut Context<UserMenu>) -> Vec<Subscription>`: what the sections read, held while
  the menu is open (each callback `cx.notify()`s). Orders: `order::PROFILE` 100, `FRIENDS` 200, `VOICE` 300
  (local mute, the volume), `VOICE_MODERATION` 400, `MODERATION` 500 (this crate's: nickname, roles, timeout,
  kick, ban), `COPY` 600. Rows: `menu::menu_section()`, `menu_row(id, label, cx)`, `menu_row_danger`,
  `menu_row_icon`, `menu_label("Moderation", cx)`; a row's action ends with `menu::close_user_menu(cx)`. The
  voice crate's sections (local mute, the volume slider: 300; voice moderation: 400) are joined by the app
  (`registry::join_features`, with these row builders handed over in a `decibell_voice::menus::MenuKit`): one
  right click, one menu; the slider applies every move at once and a drag doesn't close it.
  Gating comes from `menu::acts_for(server, &member, cx)` / `assignable_roles` (MembersTab's `actionsFor`).
- **The custom status's emoji picker**: `decibell_home::status::set_emoji_picker(cx, |EmojiPickerRequest {
  anchor, on_pick, on_close }, window, cx| -> AnyView)`, joined by the app (the composer's
  `status_emoji_picker`). The dialog builds the view when the emoji button opens it, places it as the renderer
  does (below the field, else above, else as low as the window allows; `PICKER_SIZE` 352 × 420) and drops it when
  it closes (a pick, Escape, a press outside, the dialog closing). With none registered a small chooser takes a
  typed emoji or `:shortcode:`.
- Opening things from elsewhere: `profile::open_profile(uid, server, ProfileTab::Privacy, cx)`,
  `menu::request_dialog(DialogRequest::Nickname { server, uid } | Moderation { kind, server, uid }, cx)`,
  `friends::open_conversation(uid, cx)`; the shell's `open_user_menu` / `open_profile_beside` as before.
- Screens (`home-friends-*`, `dm-*`, `profile-*`, `custom-status`, `user-menu-*`, `nickname-modal`,
  `ban-modal`) draw the whole window from `fixture.rs` (the shell's fixture plus friends in every state,
  statuses, DM pages, E2EE states); `decibell-home-fixture <dir>` exports the scenes for
  `tools/electron-captures/pages/_home.tsx`. Self-tests (`dm-*` incl. `dm-read-rules`, `friends-*`, `profile-popup`,
  `status-custom`, `user-menu-moderation`, `home-idle`) run child apps (`--check home-…`): read-only as `alice`,
  everything that changes state between two throwaway accounts registered once per run.

## Chat content (`decibell-chat-content`)

What messages contain: the `attachments`, `inflight` and `embeds` slots of the list, the lone-link rule, the
image right-click menu, the image viewer, the Transfers panel, and the picture pipeline under all of them.
`decibell_chat_content::init(cx)` installs it (the app's `registry::init_features`); screens use
`install_fixture` with a memory fetcher.

- **Every picture goes through `pictures::Pictures`** (module doc in `src/pictures/mod.rs`). Ask for a picture as
  a `PicKey` = `Source` (an attachment's thumbnail or original, a remote https URL, `Local` bytes handed over with
  `provide`) + the box it is drawn in (`Spec`: logical size, scale, `Fit::{Contain, Cover}`, upscale or not). A view
  that shows pictures for rows calls `set_wants((its entity, n), wants)` with what is near and what is on screen
  (the list's `RowSlot::near` does this for the slots); anything else drawn once calls `request(key, Some(view))`.
  Draw with `tile::Tile` (placeholder → picture → broken image in one canvas) or `Picture::paint`; never `img()`
  for chat pictures (GPUI's caches keep everything forever and animate by re-rendering at the refresh rate).
- **Rules the pipeline keeps**: decode off the UI thread at the box's device pixels (never larger than the file);
  one texel of border on every decoded picture (GPUI's atlas packs tiles edge to edge: without it a scaled picture
  bleeds its neighbours' texels) — paint with `Placement::image_bounds`; evicted pictures leave the atlas
  (`drop_image`); a picture arriving notifies the owners that want it, nothing else draws; animations run one timer
  each, only while some owner wants them on screen and the window is visible and paints them.
- **Heights before pixels**: a slot's `height()` reads only the message, the slot space, the text engine and
  stores whose change calls `invalidate_slots` (link previews, invites, the toggle, a link picture whose size only
  its pixels tell). Never make a height depend on whether a picture has loaded otherwise.
- **The player seam** (`src/player.rs`): the video / audio cards set `model.chat.video` / `chat.audio` and record
  where each video surface was drawn (`VideoHosts`); the persistent players (another crate) serve that.
- **Dates, times of day and counts** go through the private `fmt.rs` only (owner decision 2026-11-26: they will
  follow the system's regional formats); sizes, speeds and durations keep the renderer's formatting.
- **Groups fade as one layer in CSS, not in GPUI**: `.opacity()` fades every primitive on its own, so whatever is
  painted under a faded picture or label shows through it (a thumbnail over its blur, a button label over its
  fill). Paint nothing under a faded picture once it is in, or mix the faded colours yourself (the invite card's
  disabled Join).
- Screens `content-attachments`, `content-embeds`, `image-viewer`, `image-viewer-zoomed`, `transfers-panel`(`-uploads`).
  Self-tests: `content-rows` (row tops and heights of the two content channels for `tools/electron-captures/
  chat-rows.mjs`, equal to Electron's to the 1/64 px) and `content-scroll-fixture` (240 picture rows paged in as
  history at 3000 px/s) run on the fixture; `content-images-live`, `-scroll-images`, `-cache-bound`, `-gif`,
  `-viewer`, `-embed`, `-invite`, `-download` sign in to the local stack as `perf2`. Each runs as a `--check` child
  with a throwaway profile on a private empty session bus (a throwaway profile otherwise leaves an at-rest key item
  in the desktop's keyring); nothing they post stays (the GIF is deleted, link and invite messages are routed into
  the client only, nothing is joined).

## Platform (`decibell-platform`)

The desktop integration: what Electron's main process did for the window and the OS (the services table in
`HANDOFF.md` lists every one of main's jobs and who has it). `decibell-app`'s boot calls
`decibell_platform::instance::start(profile, &launch)` **before GPUI starts** (a second start hands over and exits
without opening anything), then `decibell_platform::install(window, launch, instance, profile, cx)` once the main
window is open, and draws the view it returns (the quit question) over the window's content.

- **One instance per profile directory**: an exclusive lock on `<profile>/instance.lock`, then a Unix socket
  (`<profile>/instance.sock`, or `$XDG_RUNTIME_DIR/decibell-<hash>.sock` when the path is too long; only the user's own
  connections are accepted) or, on Windows, a named pipe `\\.\pipe\decibell-<hash>` (no remote clients; the second
  start lets the first take the foreground). The listener is a thread blocked in `accept` / `ConnectNamedPipe`.
- **Windows code** (`win32.rs`) is written against `windows` 0.62 (GPUI's bindings): check each call in the crate's
  source (`~/.cargo/registry/src/*/windows-0.62.*/`), give every `unsafe` block its invariant, never `unwrap` a call
  that can fail on a real machine; it is compile-checked only by the Windows Native Check. Tests and development profiles run side by side with each other and with a shipped
  instance; a self-test that starts two children on one profile gets one instance.
- **What a start asks for** is a `Launch` (`launch.rs`: the `decibell://` arguments as `isValidDeepLink` accepts them,
  `--hotkey` as `parseHotkeyArg` does, the desktop's `XDG_ACTIVATION_TOKEN`). The first start applies its own, later
  ones arrive through the instance: links become `ServersStore::pending_invite` (`actions::servers::open_invite_link`;
  it waits there until the join dialog, decibell-servers, shows it after sign-in), hotkeys go to
  `actions::hotkeys::inject_cli` without raising the window, anything else raises it (with the token when there is one).
- **Attention** answers the model's `Cue::Flash` (the model decides: an incoming call, not under Do Not Disturb) only
  while the window is not active; the desktop ends it on activation. **Taskbar progress** observes
  `model.live.transfers`' `progress()` and sends only changes (≤ 4 / s while moving, at once when it appears, pauses or
  clears). **The quit guard** answers the title bar's close (`decibell_ui::CloseHandler`) and the desktop's
  (`on_window_should_close`) while uploads are in flight, and pauses downloads in `on_app_quit`. **Window state** is
  `window-state.json` in the profile, written on a maximise change, on losing the focus, on close and at quit (no
  timer).
- **No polling, nothing at rest**: a service that isn't in use costs nothing (the D-Bus connection for the launcher
  progress exists from a progress to its clear). Every thread is named (`decibell-instance`, `x11-idle`).
- **Auto-away's idle source** (`idle.rs`) follows GPUI's display server (`cx.compositor_name()`), never
  `XDG_SESSION_TYPE`: on X11 it is `idle/x11.rs`, the server's own `IDLETIME` counter through two SYNC alarms (idle
  at the threshold, resume at the next input; each re-arms the other) on a connection and a thread (`x11-idle`,
  blocked in `wait_for_event`) that exist only while auto-away is on; MIT-SCREEN-SAVER's `QueryInfo` per reading
  where SYNC has no `IDLETIME`. It reads a counter the server keeps: never select key or pointer events, never
  XInput2, RECORD or a grab to see activity (the hotkeys rule, in spirit). Edges are `AutoAway::poke`s; the
  reading is always a fresh read of the counter. Input in tests is faked with XTEST in a private Xvfb only
  (`idle-x11`), never on the desktop's display.
- **Markers**: development builds print `decibell: platform <event>` lines (`instance listening`, `instance forwarded …`,
  `deep-link pending`, `hotkey <action> injected`, `attention requested`, `progress <‰> [paused]`, `window-state saved …`,
  `quit-guard asks …`, `idle source x11|core|windows`, `idle x11 path sync|screensaver|unsupported: …`,
  `idle x11 idle|active`, `idle x11 stopped`); the self-tests' parents read them.
- **Self-tests** (`profile-guard`, `single-instance`, `deep-link-arg`, `hotkey-cli`, `attention`, `taskbar-progress`,
  `window-state`, `idle-x11`; `content-copy-image` lives in `decibell-app`) run the app as children with throwaway profiles on a
  private, empty session bus, and ask KWin (through `tools/perf/linux/kwin.py`) what it shows. The owner's clipboard is
  saved and put back by `content-copy-image`; nothing writes to the session keyring, the URL handlers or autostart.
- **Development builds never touch the shipped profile** (`decibell_model::core::DEVELOPMENT_BUILD`: `local-central`
  or a debug build): the default profile is `~/.cache/decibell-dev/gpui-default` (`development_profile_dir`), and
  `check_profile` refuses `~/.config/decibell` even when `DECIBELL_USER_DATA_DIR` names it (the app exits 2, the core
  refuses to start, `decibell-headless` refuses in any build). Self-tests still give every child its own temporary
  profile.
## Media playback (`decibell-media`, `decibell-chat-content/src/playback.rs`)

Video and audio attachments played in their cards (`docs/media-design.md`: the renderer's rules, the codec
matrix, the engine, the no-pop rule and its tests).

- **The engine has no GPUI in it**: `decibell_media::Player::open(Open { source, video: Some(hub), key, output,
  start_at, play, volume, muted, on_change })`, then `play` / `pause` / `toggle` / `seek(micros)` /
  `set_volume(v, muted)` / `set_visible(bool)`, `status()` (phase, duration, the picture's size and decoder),
  `position()`, `viewer()` for a `VideoView`. Dropping it fades the sound out and closes everything. `on_change`
  is called from its threads: send into one channel and read `status()` on the UI thread
  (`Playback::refresh`), never one task per call.
- **Who owns players**: `playback::Playback` (a global entity), one video and one audio, following
  `model.chat.video.active` / `chat.audio.active`. A view never holds a `Player`. To start something, set the
  active attachment (`crate::player::play_video` / `play_audio`); to stop, set it to `None`.
- **Where bytes come from**: `MediaReader::blocking()` behind the player's AVIO (range reads, sealed files
  opened in memory). Never a temp file, never a URL handed to FFmpeg. Fixtures and self-tests replace it with
  `Playback::set_source_provider`.
- **Sound is its own stream on the system's default output** (`decibell_media::system_output()`), as the
  renderer's media elements play on Chromium's default sink. Never route it through the voice engine, its
  mixer or its device streams, and never touch those to play a file.
- **No pops is a property of `render.rs`, kept by its tests**: whatever is handed to a device is
  `sample × shape(envelope) × volume` with both slewed. Anything that would start, stop, skip or re-level
  sound goes through the renderer's controls (`set_playing`, `set_gate`, `set_volume`, a new epoch through
  `Feed::begin`), never around them; a new transition gets a sample-exact test there and a line in HANDOFF §6
  for the owner's ear.
- **Frames**: a picture is a present of the kept scene (the video crate), never a render. The list redraws for
  the controls only: `Playback`'s 250 ms tick runs while a running time is on screen (the audio card's row
  drawn, the video's controls shown), and stops otherwise. The video's controls hide 3 s after the pointer
  rests; with them hidden a playing video costs the list 0 frames (`media-video-play`).
- **Tests are inaudible**: unit tests render into `TestOutput::recording` (every sample kept, with the instant
  it "played"); self-tests and measurements run with `DECIBELL_MEDIA_OUTPUT=null` (a thread that pulls blocks
  in real time and discards them; `fixture_parent` sets it). Nothing in a test opens the sound card.
- **Test media** are synthetic and committed (`crates/decibell-media/tests/assets`, `make.py`): a barcode of
  the frame number and a click track. A new container or codec in the matrix gets a file there, a row in
  `every_file_of_the_matrix_plays_to_its_end`, its components in `tools/build-ffmpeg.sh` **and** in
  `decibell-app/build.rs`'s lists, and a line in `media-design.md` §2 saying that Chromium plays it.
- Self-tests `media-idle`, `media-video-play`, `media-audio-play`, `media-persist` (the content fixture's
  `#clips` channel, memory sources; `DECIBELL_MEDIA_CLIP=<file>` makes the video cards play that file) and
  `media-live` (the local stack as `perf2`: uploads a clip to `#off-topic`, plays it from the attachment
  service, deletes the message).

## Release (`decibell-release`) and packaging (`packaging/`)

What a released build does on its own and how it is packed: `PACKAGING.md`.

- **Two flavours of `decibell-app`**: `dev` (the default: screens, self-tests, fixtures, the release
  crate's test hooks) and `ship` (`--no-default-features --features ship`). A screen, self-test, check,
  fixture or test-only switch goes behind `dev` (in the app) or `fixtures` (in a feature crate) so the
  ship build doesn't link it; `packaging/check-ship.sh` fails on development strings in the binary
  (`DECIBELL_CENTRAL`, `DECIBELL_TEST_`, `DECIBELL_DEV_*`, self-test names). Check both flavours build
  without warnings (`cargo check --release -p decibell-app --no-default-features --features ship`): a
  module that only the fixtures reach gets `cfg` gates, or `#![cfg_attr(not(feature = "fixtures"),
  allow(dead_code))]` with a comment saying so (`decibell-chat/src/live.rs`).
- **The version** is `decibell_release::VERSION` (`crate::VERSION` in the app): never `CARGO_PKG_VERSION`.
- **The release crate is GPUI-free** and blocking (its HTTP client runs on threads that exist only while
  they work, and in a panic hook); the app's `updater` / `crash` modules are the GPUI glue. Nothing in it
  runs at rest: no timer, no thread between checks or reports.
- **Hosts**: the updater may contact `github.com` and its two download hosts only, the crash reporter the
  DSN's host only (`http::Rule`); tests use loopback fixtures through `test-hooks` (plain HTTP to
  127.0.0.0/8, compiled out of `ship`). Never GitHub's API, Sentry or a real feed from a test.
- **Windows**: the executable's resources are written by `build/winres.rs` (no resource compiler); a new
  resource goes there. The AppUserModelID is `decibell_release::APP_USER_MODEL_ID`, set at start and
  carried by the installer's shortcut (`--create-shortcut`): change both together.
- **Tests**: `cargo test -p decibell-release`; `packaging/tests/release_e2e.py --binary <dev release build>`
  (the updater, apply and crash reports through the real binary); `packaging/tests/ship-isolated.sh`
  (a ship binary on a private bus and keyring, network calls logged).

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
11. **A dirty cached view re-renders every cached view inside it.** `prepaint_view` / `paint_view`
    (`crates/gpui/src/view.rs:562`, `:632` at the pin) set `window.refreshing` while a dirty view re-renders, and
    `refreshing` defeats every nested cache. A notify marks all ancestors dirty, so a key typed in the composer
    re-renders the chat pane, which re-renders the message list beside it (measured, `composer-typing-cost`: 44
    list frames for 43 keys; the list's frame then costs ~0.7 ms in a debug build). Not setting `refreshing` there
    (two lines) gave 0 list frames for 43 keys and halved the key → frame time, with `chat-*`, `shell-navigate`,
    `shell-idle`, `idle-frames` and `themes` passing; not applied (it changes caching for every view).
12. `img(…)` animates a GIF only when it has an `.id(…)` and only while the window is active.
13. With nothing focused, GPUI dispatches key and modifier events to the window's root dispatch node only
    (the root view's element, above every listener a view can register). The app's `Root` therefore holds
    the focus whenever nothing else does (and takes it back through `on_focus_lost`, of which a window has
    one): its listeners (the focused hotkey fallback, Tab) see every key. A test window of your own that
    needs keys focuses something first.
14. Focus changes reach their listeners (`on_focus`, `on_blur`, `InputEvent::Blur`) when the window next
    draws, and a mouse event is dispatched to the frame last drawn while a key event draws a pending frame
    first. A self-test that presses and releases with no frame between sees the release handled before the
    blur, and a click right after a change can reach the old frame's handlers (a window in the background
    may not draw on its own): draw what is pending first (`draw_pending` in `decibell-settings`'
    self-tests: a modifiers event that names no key) and put one between press and release.
15. **A key with nothing focused reaches only the window's root node** (`focus_node_id_in_rendered_frame` falls back
    to node 0, the root view): no element's `on_key_down` below it sees it. The app's `Root` holds the focus
    whenever nothing else does (13), so its listeners see every key; `App::intercept_keystrokes` runs before
    everything (key bindings, the focused element) and its `KeystrokeEvent` carries no physical key code, so
    app-wide handling that must defer to the focused element or read the physical key (type-to-focus, the
    hotkey fallback) belongs on the root's element instead.
14. **KaTeX's font files mark every face Regular** (OS/2 `fsSelection` 0x0040); fontconfig (Chromium) takes the
    slant from the style name, fontdb (GPUI) from `fsSelection`: upright `KaTeX_Main` resolved to the italic face
    and italic `KaTeX_Math` got a synthetic slant on top. `decibell-text` marks the five italic faces before it
    hands them over (`math::mark_italic`; test `katex_faces`).

## Servers (`decibell-servers`)

The community screens that are no settings window: Browse (`Slot::Browse`), the deep-link join dialog, Invites
(`modal::INVITE_MANAGE`), Invite friends (`modal::INVITE_FRIENDS`), the create-channel dialog (answering
`ShellRequests::create_channel`), dragging the channel list with the Sync / Keep / Cancel prompt, and the welcome
banner (the chat pane's `slots(cx).welcome`). `decibell_servers::init(cx)` registers all of it; `src/lib.rs` maps
the files. What stays the shell's: the server-name dropdown, the channel list's context menu, the delete and leave
confirmations, the kicked / banned notice.

- **A deep link** (the OS's `decibell://` handler, a link that started the app) goes through
  `decibell_servers::open_invite_link(url, cx)`: the model's grammar (`invite_link.rs`) sets
  `ServersStore::pending_invite`, and the join dialog shows now or as soon as the main layout does (a link received
  before sign-in waits for it). Links are code-only; older `host:port/code` ones still join without central.
- **Personal invites** (`invite_friends.rs`): the picker filters friends by central's `friends_in_community`
  answer plus the loaded roster, **never by asking the community** (owner rule); each invite is a request with
  its own id answered on `model.answers` (8 s without an answer is a failure), then `send_private_message` as
  the renderer sends it (no optimistic bubble, the DM draft untouched).
- **Dragging the channel list** (`reorder.rs`, the shell's `ChannelReorder`): our own press / move / release on
  window listeners that exist only while a press is held (not GPUI's drag, whose every move redraws the whole
  window): each move redraws the sidebar alone, nothing draws while the pointer holds still, Escape cancels
  (a keystroke interceptor held only while dragging), auto-scroll steps on pointer moves (no timer). The drop
  rules are pure functions with unit tests (`drop_point`, `reorder`, `resolve_drop`); the order is applied at once
  (`Channels::set_all`) and sent whole, the community's broadcast settles it. A drop into another syncing category
  stores a `SyncPrompt` in the drag state; `SyncPromptOverlay` shows it.
- **Inputs** are the renderer's `<input>` boxes around `decibell-text`'s `TextInput` (`ui::input_box`): each look
  (background, radius, padding, `text-sm` / 13 px, monospace with a sans placeholder, the plain placeholder of an
  input without a `placeholder:` class) is an `InputLook`. Autofocus happens in the next render (it has the window);
  Escape in a dialog's field closes the dialog (`useEscapeToClose` listens on the document).
- **Windowed lists**: the directory's grid by rows (`auto-fill` counts columns at the 360 px maximum, CSS Grid
  §7.2.3.2; a row is its tallest card, descriptions measured once per list and width), the invite list (each row's
  meta line packed as `flex-wrap` would), the friends picker (44 px rows). A list's width is measured by a probe
  in its rows and re-laid out when it changes (a resize, the scrollbar).
- **Screens** (`--screen browse`, `join-dialog-*`, `invite-manage*`, `invite-friends`, `server-menu-*`,
  `leave-confirm`, `create-channel-*`, `channel-menu`, `sidebar-dragging-*`, `category-sync-prompt`, `welcome-*`)
  are the shell's fixture plus `decibell_servers::fixture` (a directory, invites, more friends, the ownership or
  roles a menu is gated on, a syncing category, empty channels) and what the user did (`ScreenState`: a typed
  search, a create that is out; `ShellRequests::open_sidebar_menu`; `reorder::state(cx)` for a drag).
  `decibell-servers-fixture <dir>` exports them for the Electron pages (`pages/_servers.tsx`, which replays the
  same events and clicks, right-clicks and drags as the user would).
- **Self-tests** run the app as children against the local stack, as Settings' do. Joining and leaving use
  throwaway accounts (`sv<hex>`); as `carol` (Design Lab's owner) they create only channels named
  `sv-<pid>-…` and their own invites, delete them again (and what an earlier failed run left), reorder only among
  them, and assert the seeded order unchanged. `invite-manage-live` runs its child under XWayland and checks Copy
  link only on an empty clipboard, which it empties again (`wl-copy --clear`); otherwise it says it skipped.

## Voice (`decibell-voice`)

The voice view, streams, Go Live, the voice menus and DM calls (`crates/decibell-voice/src/lib.rs` maps the files).
What it registers: `Slot::Voice` (the stage), `Slot::DmCallStage` (the call stage the DM pane shows above its message
list during a call with that conversation's peer), four overlays (mini-player, Go Live, the user panel's popovers, the
incoming call) and the user panel's voice controls (`register_user_panel_action`). The user menu is the people crate's:
the voice sections (`menus::user_menu_mute` / `user_menu_volume` / `user_menu_moderation`, `watch_user_menu`) are
joined into it by the app (`registry::join_features`), so neither crate depends on the other.

- **Layout in observers, never in `render`.** The stage and the filmstrip recompute their layout (`layout.rs`: the
  renderer's `fitGrid` / `gridCapacity` / "+N more" / hidden-speaker swap, unit-tested) when a store they show changes
  or their measured size moves, push each tile its data (a tile notifies only when its data changed) and notify
  themselves only when their own layout moved. A tile subscribes to its own keyed events (speaking for its uid, its
  person, its stream's poster and first picture).
- **A change renders the views on its path, not their clean neighbours** (GPUI patch `0014`; at the pin every cached
  view under a dirty one rendered with it): a tile's notify marks its ancestors dirty (the stage, the pane, the main
  layout, the window's content), those render, and every other tile and panel is replayed from its cache
  (`decibell --selftest render-cost` has the numbers). So keep `render` cheap on the path (no layout maths, no
  allocation beyond the elements), and keep a view that changes often small and low in the tree. A cached view is
  replayed only while its bounds, content mask, inherited text style, opacity and rem size are what they were: a
  parent that moves or restyles a child renders it.
- **Video**: a view that shows a stream holds a `decibell_video::Viewer` (`players::viewer(uid, cx)`) and paints
  `VideoView`; the hub decodes a stream while any viewer of it exists (one decoder for any number of views) and the
  core delivers its frames only then (the app's `HubCallbacks::interest`). `players::Players` holds the loaded stream's
  viewer (`watching.pip`, the renderer's persistent player) so moving between the stage, the focused view and the
  mini-player never restarts the decoder; create the new viewer before dropping the old one. The spinner shows until
  `video_events::StreamStates` has the stream's first picture; once it had one, `StreamState::stalled` (no picture
  for 3 s: the hub's `Stalled` / `Resumed`, no timer) draws `scrim::reconnecting` over it: every place that paints a
  `VideoView` draws both.
- **Timers with a live reason only**: the focused view's overlay hide (2.5 s, while shown; in fullscreen the same
  timer hides the mouse cursor until it moves, `focus::rest_cursor`, GPUI patch `0013`), the stats overlay's 500 ms
  refresh (while open), the stream-audio picker's 2 s app list (while shown, the renderer's poll), the hidden-speaker
  swap (while a hidden speaker waits), the call's 1 s duration (while live and shown), the incoming card's "answered
  elsewhere" 3 s. The ringing outline breathes by re-presenting a retained sprite (`TimedTransformation`, ≤ 30 fps),
  never by re-rendering; the mini-player's drag, spring and entrance request frames only while they move.
- **Codec capabilities are the model's, loaded by its one action**: `actions::voice::init_codec_caps` (never at start:
  the encoder probe opens encoder sessions; any number of callers, one load). `codecs::ensure_caps` calls it at Go
  Live's first open and the first voice session, Settings → Codecs when it opens. `voice.codecs.encoders` has each
  encoder's backend, label and limits (a software encoder's pixel-rate ceiling: presets past it are greyed);
  `codecs::Encoders` mirrors it for the Go Live views (tests pin a fake answer there). Decode capabilities come from
  the video crate's probe, which the app runs after its first frame (`crates/decibell-app/src/video.rs`). Never call
  the core's probe functions from a view.
- **Self-tests** (`selftests.rs`) run the app as `perf3` and a `decibell-headless` peer as `perf4` (both have E2EE keys;
  passphrase `decibell-m3-test-passphrase`; `perf5` is a second streamer for measurements, `tools/dev/local-stack.sh
  top-up`), everyone muted and deafened before joining. The headless `stream-file` step (with `stream-pause` /
  `stream-resume`: a streamer whose frames stop arriving) and `tools/perf/streambot.sh` stream `.dbv` recordings into
  a channel through the real path.
  `DECIBELL_VOICE_TRACE=1` logs every render of the voice views (which view drew a frame).

## Search and channel header (`decibell-search`)

The text channel header's search box and the results panel (`features/search/`, spec
`docs/superpowers/specs/2026-10-14-message-search-design.md`). The header itself is the shell's
(`server_pane.rs`: `#`, name, the E2EE badge, the topic in `tracking-normal`, the members toggle, then the
244 px search slot with `ml-4`, centred over the 260 px members column); `decibell_search::init(cx)` fills
`Slot::HeaderSearch` with [`SearchBox`] and `Slot::RightPanel` with [`RightPanel`], and binds Ctrl+F / ⌘F
(`FocusSearch` → the store's focus nonce → the box that is drawn).

- **The search is the model's** (`decibell_model::chat::search::SearchStore`: request, supersede by `seq`,
  refusals retried after `retry_after_ms` up to three times, the 8 s "no answer", paging, the per-community
  drafts). The views only read it and call `run` / `set_sort` / `load_more` / `retry_search` / `close` /
  `set_jumped`; `actions::chat::go_to_message` is Jump. **Nothing about a search is stored on the device**:
  the drafts live in the store's memory, nothing goes to `ui.json` (`search-nothing-stored` greps the profile).
- **The grammar** (`tokens.rs`: `trailing_token`, `resolve_token`, `chip_tokens`, `on_text`, …; `menu.rs`: the
  menu's rows; `calendar.rs`; `highlight.rs`) is the renderer's, checked against the real TypeScript:
  `tools/electron-captures/pages/search-grammar.tsx` runs `searchTokens.ts`, `searchQuery.ts`, `highlight.tsx`
  and `DatePicker` on a table of inputs into `src/renderer_grammar.json`, which `tests/renderer_grammar.rs`
  compares with the ports and the model's (`fold`, `search_words`, `parse_day`, `with_filter`,
  `to_search_args`). Known difference: the model's `fold` maps stroked Latin letters (Đ, Ł, Ø…) to their
  base and keeps Greek accents; the renderer (NFD) keeps đ, ł, ø and drops the accents (`FOLD_DIFFERS`).
- **The box** is a cached view: a key re-renders it and its field (`InputState`, one line, `line-height:
  normal` from the face's rounded ascent + descent, as Chromium centres an `<input>`'s text). Chips are
  elements before the field in a row that scrolls sideways (`overflow-x`) with the box's ground fading over
  a cut-off chip; Backspace at the field's start removes the last chip. Its keys run in the capture phase
  (`capture_key_down`, and `capture_action` for Tab / Shift+Tab, which `decibell_ui` binds to focus moves),
  so the field never sees Escape, Enter, the arrows in a picker. The menu is `deferred`, right-aligned 6 px
  under the box, never takes the focus (`capture_any_mouse_down` + `prevent_default`); a press outside the
  box and its menu blurs the field (`on_mouse_down_out`), as a page's click does. `from:` asks the server's
  member search (`search_members`, 250 ms debounce, seqs apart from other member searches) for offline
  members past the loaded roster.
- **The results** ([`ResultsView`], cached, alive only while the panel shows: closing drops it and its rows)
  are a windowed `ScrollArea`: every result's card height is computed when its page arrives (one paragraph
  layout at the column's width, then dropped; `result_row::card_height`), only the rows on screen keep their
  bodies. Heights reproduce the CSS: the scrollbar's 10 px decide the width (`max_offset` of the last frame),
  a channel heading's `mt-3` collapses with the card above's `mb-1.5`. A body is one `decibell-text`
  paragraph built from the message's rich-text document with the matched words as `<mark>` chips
  (`bg-accent-mid text-text-bright rounded-sm`), `line-clamp-6`; file names are marked too (`runs::Runs`, a
  label of several styled runs). The next page is asked for when the end is 400 px away (the renderer's
  sentinel). Below 1024 px (`is_narrow`) the panel floats 360 px wide over the chat; Jump closes it there.
- **Differences from the renderer** (listed, not hidden): no `tabular-nums` (decibell-text has no font
  features); result bodies draw code blocks and display maths as monospace text in the paragraph; media
  tiles are the placeholder (dark tile, or the kind's icon when the renderer has no source) until the
  attachments crate draws pictures here; the disabled "Relevant" segment has no title tooltip (the kit's
  `Segment` has none); Escape closes the panel when the focus is nowhere, in the panel or on the message list
  (GPUI can't tell whether the focused element is a text field, which the renderer exempts); menu text of
  13 px sits up to 1 px higher than Chromium's (line boxes of 19.5 px).
- **GPUI at the pin re-renders every cached view under a dirty one**: a key dirties the views up to the root,
  so typing in the box lays the message list out again (`search-typing-cost` fails on that count until GPUI
  patch 0013, "keep nested view caches", is in; frames per key are 1 either way).
- **Screens** (`header-text`, `header-encrypted`, `header-long-topic`, `search-box-empty`, `search-box-suggest`
  (`-from`, `-in`, `-has`), `search-box-chips`, `date-picker`, `search-results`, `-empty`, `-refused`,
  `-narrow` at `--size 900x700`) are the shell on the chat fixture plus `decibell_search::fixture` (a store
  whose sender goes nowhere: `SearchStore::set_sender`; the answer routed through the real router; "today"
  pinned with `format::PinnedToday`). `decibell-search-fixture <dir>` exports them for the Electron pages
  (`pages/_search.tsx`: the same events, the draft, the search and its answer, `Date` pinned to the same day).
- **Self-tests** (`search-*`, `header-members-toggle`) run the app as a child signed in as perf1 against the
  local stack, read-only, pacing searches to the community's bucket (6, then 1 a second). Probes inside
  padded elements go through `crate::probe` (the shell's canvas has no insets: in a padded flex container
  it would sit after the padding).

## Windows

The client ships on Windows too, and no session on Linux can run it there. What a crate author does so that new
code works on Windows the first time the owner starts it:

- **Type-check for Windows before pushing**: `flock ~/.cache/decibell-dev/heavy-build.lock tools/win-check.sh`
  (the whole workspace for `x86_64-pc-windows-msvc`, on Linux, about a minute warm). The Windows Native Check
  then compiles for real and runs the unit tests and self-tests a GPU-less runner can run
  (`HANDOFF.md` §1b says where its logs are). A row that only compiles on Windows is "needs owner", with its
  step in `docs/windows-owner-checks.md`.
- **No Linux tool or path in product code**: no `/proc`, `/tmp`, `$HOME`, `xdg-*`, D-Bus, signals. Ask GPUI
  (`open_url`, `reveal_path`, `prompt_for_paths`, the clipboard, credentials) or `decibell-platform`; where the
  platforms differ, write both sides behind `cfg` in one module with one neutral API, and make the other side a
  real implementation, never a stub that answers "unsupported".
- **A self-test that needs a Linux helper says so and skips** (`cx.log("SKIPPED: …")`) instead of failing, and
  a fixture-only self-test must need nothing but the window: those are what the Windows runner executes.
- **The `windows` crate**: the version GPUI uses (0.62, see `Cargo.lock`), as a `cfg(windows)` target
  dependency with only the features used; anything that touches GPUI's Direct3D device goes through
  `gpui_windows::windows` and holds `D3d11Devices::lock_context()` for a sequence of context calls off the main
  thread. Every `unsafe` block states its invariant; no `unwrap` on a call that a real machine can fail (a
  window that closed, access denied on an elevated window, a driver without the feature).
- **Frames and wakeups are the same rule**: GPUI's Windows backend sleeps completely when no window asks for a
  frame (patch `0021`). Anything that calls `request_animation_frame` or notifies a view on a timer keeps a
  thread waking at the refresh rate there exactly as it keeps the compositor busy on Linux.
- **Fonts**: a bundled family's name must not contain a weight, width or slope word ("Expanded", "Condensed",
  "Light", "Italic", …): DirectWrite takes it out of the name and files the face under the shorter family
  (`tools/gen-fonts.py`). `boot` checks every bundled face on both platforms.
- **Wheel steps, word stops, key codes** differ by platform on purpose (Chromium's conventions:
  `decibell_ui::scroll::PX_PER_GPUI_LINE`, `decibell_text::input::boundaries`, `KeyCode::Windows`): tests take
  the platform's value from the code, never a Linux number.
