//! The chat self-tests: the list driven in a real window (`decibell --selftest chat-history`…).
//! Each opens its own window on the fixture channel and closes it.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_ui::scroll::{ScrollAnimator, ScrollKind};
use decibell_ui::{ActiveTheme as _, FontRole, TitleBar, focus_navigation, outer_window_size, window_frame};
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, Render, ScrollDelta,
    ScrollEvent, StyleRefinement, TitlebarOptions, TouchPhase, Window, WindowBounds, WindowDecorations, WindowOptions, div, point,
    prelude::*, px, size,
};

use crate::fixture::Which;
use crate::list::MessageList;
use crate::row::RowPos;
use crate::screens::FixturePane;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "chat-history", about: "a page above, a live message and a page mid-wheel move the row being read 0 px; the bottom follows", run: |cx| Box::pin(history(cx)) },
        SelfTest { name: "chat-switch", about: "leaving a channel and coming back restores its exact position in the first frame", run: |cx| Box::pin(switch(cx)) },
        SelfTest { name: "chat-resize", about: "a width change keeps the row being read where it is", run: |cx| Box::pin(resize(cx)) },
        SelfTest { name: "chat-selection", about: "drag across rows, autoscroll, double / triple click, Shift+click, the copied text, the scroll keys", run: |cx| Box::pin(selection(cx)) },
        SelfTest { name: "chat-scroll-spec", about: "wheel notches and keys through the real element follow docs/wheel-scroll.md's model", run: |cx| Box::pin(scroll_spec(cx)) },
        SelfTest { name: "chat-rows", about: "every row's top and height on both fixture channels (written to $DECIBELL_CHAT_ROWS_DIR for the Electron comparison)", run: |cx| Box::pin(rows(cx)) },
        SelfTest { name: "chat-idle", about: "a channel open and at rest draws no frame for 10 s", run: |cx| Box::pin(idle(cx)) },
    ]
}

/// What a test window draws: the app's root without the shell and without the toast stack
/// (frame, title bar, cached content): the fixture's model has no core, and the toasts of its
/// failed requests would come and go over the list under test.
struct TestRoot {
    content: AnyView,
}

impl Render for TestRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = focus_navigation(div())
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(TitleBar::new("Decibell chat test"))
            .child(div().flex_1().min_h_0().child(self.content.clone().cached(StyleRefinement::default().size_full())));
        window_frame(content, window, cx)
    }
}

pub(crate) struct Harness {
    pub cx: TestCx,
    pub pane: Entity<FixturePane>,
}

impl Harness {
    /// A window like the main one (1280 × 800) on a fixture channel.
    pub fn open(cx: &TestCx, which: Which) -> Result<Harness> {
        Self::open_pages(cx.app(), which, 1)
    }

    pub fn open_pages(app: &gpui::AsyncApp, which: Which, pages: usize) -> Result<Harness> {
        let (handle, pane) = app.update(|cx| -> Result<(AnyWindowHandle, Entity<FixturePane>)> {
            let bounds = Bounds::centered(None, outer_window_size(size(px(1280.), px(800.))), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(600.))),
                titlebar: Some(TitlebarOptions { title: Some("Decibell chat test".into()), appears_transparent: true, traffic_light_position: None }),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("decibell".into()),
                inactive_frame_interval: None,
                ..Default::default()
            };
            let mut pane = None;
            let window = cx.open_window(options, |_, cx| {
                let view = cx.new(|cx| FixturePane::with_pages(which, pages, cx));
                pane = Some(view.clone());
                cx.new(|_| TestRoot { content: view.into() })
            })?;
            Ok((window.into(), pane.context("no pane")?))
        })?;
        Ok(Harness { cx: TestCx::new(app.clone(), handle), pane })
    }

    pub fn close(&self) {
        let _ = self.cx.update(|window, _| window.remove_window());
    }

    pub fn list(&self) -> Result<Entity<MessageList>> {
        self.cx.update(|_, cx| self.pane.read(cx).list.clone())
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MessageList, &mut Context<MessageList>) -> R) -> Result<R> {
        let list = self.list()?;
        self.cx.update(|_, cx| list.update(cx, f))
    }

    fn input(&self, event: PlatformInput) -> Result<()> {
        self.cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        })
    }

    fn wheel(&self, lines: f32) -> Result<()> {
        let position = self.center()?;
        self.input(PlatformInput::Scroll(ScrollEvent {
            position,
            delta: ScrollDelta::Lines(point(0., lines)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
            source: Default::default(),
        }))
    }

    fn center(&self) -> Result<Point<Pixels>> {
        self.with(|list, _| list.bounds().center())
    }

    fn mouse_move(&self, position: Point<Pixels>, pressed: bool) -> Result<()> {
        self.input(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: pressed.then_some(MouseButton::Left), modifiers: Modifiers::default() }))
    }

    fn mouse_down(&self, position: Point<Pixels>, click_count: usize, shift: bool) -> Result<()> {
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers { shift, ..Modifiers::default() },
            click_count,
            first_mouse: false,
        }))
    }

    fn mouse_up(&self, position: Point<Pixels>) -> Result<()> {
        self.input(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 }))
    }

    fn key(&self, keystroke: &'static str) -> Result<()> {
        let keystroke = Keystroke::parse(keystroke)?;
        self.input(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false, code: gpui::KeyCode::Unknown }))?;
        self.input(PlatformInput::KeyUp(KeyUpEvent { keystroke, code: gpui::KeyCode::Unknown }))
    }

    /// Waits until the list has measured every row and stopped drawing.
    pub async fn rest(&self) -> Result<Frames> {
        for _ in 0..200 {
            let frames = self.cx.settle().await?;
            if self.with(|list, _| list.rows_unmeasured() == 0 && !list.is_animating())? {
                return Ok(frames);
            }
        }
        anyhow::bail!("the list never came to rest")
    }

    /// Scrolls so that the viewport's bottom is `above` px over the content's end, at once.
    fn read_above_bottom(&self, above: f64) -> Result<()> {
        self.with(|list, cx| {
            let (_, max) = list.scroll_range();
            let position = list.scroll_position(cx);
            list.scroll_by_precise(max - above - position, cx);
        })
    }

    /// The first fully visible row and its screen y.
    fn reading(&self) -> Result<(i64, f32)> {
        self.with(|list, _| {
            let ids = list.visible_ids();
            ids.iter().filter_map(|id| Some((*id, list.row_screen_y(*id)?))).find(|(_, y)| *y >= 0.).context("no row on screen")
        })?
    }

    fn live(&self, id: i64, content: &str) -> Result<()> {
        self.cx.update(|_, cx| {
            let source = self.pane.read(cx).source.clone();
            source.live_message(id, content, cx);
        })
    }

    fn y_of(&self, id: i64) -> Result<f32> {
        self.with(|list, _| list.row_screen_y(id))?.with_context(|| format!("row {id} is not in the list"))
    }
}

async fn history(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = history_in(&h).await;
    h.close();
    result
}

async fn history_in(h: &Harness) -> Result<()> {
    h.rest().await?;
    ensure!(h.with(|list, _| list.at_bottom())?, "a channel opens at its newest message");

    // Reading 800 px above the bottom: a live message moves nothing.
    h.read_above_bottom(800.)?;
    h.rest().await?;
    let (reading, before) = h.reading()?;
    h.live(20_000, "a live message, 800 px below what is being read")?;
    h.rest().await?;
    let after = h.y_of(reading)?;
    ensure!(after == before, "a live message moved the row being read by {} px", after - before);
    ensure!(!h.with(|list, _| list.at_bottom())?, "reading history is not at the bottom");

    // Further up, into the paging zone; the next page takes 150 ms to arrive.
    h.cx.update(|_, cx| h.pane.read(cx).source.latency.set(Duration::from_millis(150)))?;
    let pages = h.cx.update(|_, cx| h.pane.read(cx).source.pages.get())?;
    let rows = h.with(|list, _| list.row_count())?;
    let (min, _) = h.with(|list, _| list.scroll_range())?;
    h.with(|list, cx| {
        let position = list.scroll_position(cx);
        list.scroll_by_precise(min + 600. - position, cx);
    })?;
    h.cx.sleep(Duration::from_millis(60)).await;
    let (reading, before) = h.reading()?;
    ensure!(h.with(|list, _| list.row_count())? == rows, "the page must not have arrived yet");
    // Nearing the top asked for the page by itself.
    h.rest().await?;
    let (pages_after, rows_after) = (h.cx.update(|_, cx| h.pane.read(cx).source.pages.get())?, h.with(|list, _| list.row_count())?);
    ensure!(pages_after > pages && rows_after > rows, "nearing the top pages by itself ({rows} → {rows_after} rows)");
    let after = h.y_of(reading)?;
    ensure!(after == before, "an older page moved the row being read by {} px", after - before);
    h.cx.log(format_args!("older page ({} rows above): row {reading} stayed at y = {before} px", rows_after - rows));

    // A live message while reading history: nothing moves, and far enough up the pill shows.
    h.cx.update(|_, cx| h.pane.read(cx).source.latency.set(Duration::ZERO))?;
    h.read_above_bottom(1900.)?;
    h.rest().await?;
    let (reading, before) = h.reading()?;
    h.live(20_001, "a live message while reading history")?;
    h.rest().await?;
    let after = h.y_of(reading)?;
    ensure!(after == before, "a live message moved the row being read by {} px", after - before);
    ensure!(h.with(|list, _| list.shows_pill())?, "\"Jump to present\" shows while far above the bottom");
    h.cx.log(format_args!("live message while reading: row {reading} stayed at y = {before} px, the pill is up"));

    // Back to the present; a message arriving there stays flush with the bottom.
    h.with(|list, cx| list.jump_to_present(cx))?;
    h.rest().await?;
    ensure!(h.with(|list, _| list.at_bottom() && !list.shows_pill())?, "jump to present lands at the bottom");
    h.live(20_002, "a live message at the bottom\nwith a second line")?;
    h.rest().await?;
    let flush = h.with(|list, cx| {
        let (_, max) = list.scroll_range();
        let height = f32::from(list.bounds().size.height);
        let bottom = list.row_screen_y(20_002).zip(list.row_height(20_002)).map(|(y, row)| y + row);
        (list.scroll_position(cx) == max, bottom, height)
    })?;
    // The row's height is in 1/64 px; on screen its bottom edge is the viewport's.
    ensure!(flush.0 && flush.1.is_some_and(|bottom| (bottom - flush.2).abs() < 0.5), "a new message at the bottom stays flush: {flush:?}");

    // A page arriving during a wheel animation: the content moves continuously.
    h.cx.update(|_, cx| h.pane.read(cx).source.latency.set(Duration::from_millis(70)))?;
    let (min, _) = h.with(|list, _| list.scroll_range())?;
    h.with(|list, cx| {
        let position = list.scroll_position(cx);
        list.scroll_by_precise(min + 1000. - position, cx);
    })?;
    h.cx.sleep(Duration::from_millis(30)).await;
    let (pages, rows) = (h.cx.update(|_, cx| h.pane.read(cx).source.pages.get())?, h.with(|list, _| list.row_count())?);
    let (reading, start) = h.reading()?;
    let started = Instant::now();
    let mut samples = vec![start];
    let mut notches = 0;
    while started.elapsed() < Duration::from_millis(700) {
        // Four notches up, 60 ms apart (docs/wheel-scroll.md §5.5).
        if notches < 4 && started.elapsed() >= Duration::from_millis(60 * notches) {
            h.wheel(3.)?;
            notches += 1;
        }
        h.cx.sleep(Duration::from_millis(4)).await;
        if let Ok(y) = h.y_of(reading) {
            samples.push(y);
        }
    }
    h.rest().await?;
    let (pages_after, rows_after) = (h.cx.update(|_, cx| h.pane.read(cx).source.pages.get())?, h.with(|list, _| list.row_count())?);
    ensure!(pages_after > pages && rows_after != rows, "the wheel crossed the paging zone and a page arrived mid-animation");
    let end = h.y_of(reading)?;
    ensure!(end - start == 480., "four notches travel exactly 480 px across the page's arrival, not {}", end - start);
    let steps: Vec<f32> = samples.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let worst = steps.iter().copied().fold(0f32, f32::max);
    ensure!(steps.iter().all(|step| *step >= 0.), "the content never moved backwards while the page arrived");
    ensure!(worst <= 60., "no jump while the page arrived (largest step between samples: {worst} px)");
    h.cx.log(format_args!("page mid-wheel: 480 px exactly, {} samples, largest step {worst} px, never backwards", samples.len()));
    Ok(())
}

async fn switch(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = switch_in(&h).await;
    h.close();
    result
}

async fn switch_in(h: &Harness) -> Result<()> {
    h.rest().await?;
    h.read_above_bottom(1234.5)?;
    h.rest().await?;
    let a = h.cx.update(|_, cx| h.pane.read(cx).source.clone())?;
    let (reading, before) = h.reading()?;
    let saved = h.with(|list, cx| list.position(cx))?;
    let visible = h.with(|list, _| list.visible_ids())?;

    // A → B.
    h.cx.update(|_, cx| h.pane.update(cx, |pane, cx| pane.switch(Which::Features, cx)))?;
    h.rest().await?;
    ensure!(h.with(|list, _| list.at_bottom())?, "a channel never visited opens at the bottom");

    // B → A: the very first frame shows A where it was left.
    let frames = h.cx.frames()?;
    h.cx.update(|_, cx| h.pane.update(cx, |pane, cx| pane.remount(a.clone(), cx)))?;
    let mut first = None;
    for _ in 0..200 {
        h.cx.sleep(Duration::from_millis(2)).await;
        if h.cx.frames()?.since(frames).drawn >= 1 {
            first = Some((h.y_of(reading)?, h.with(|list, _| list.visible_ids())?, h.cx.frames()?.since(frames).drawn));
            break;
        }
    }
    let (y, ids, drawn) = first.context("no frame after the switch")?;
    ensure!(y == before, "frame {drawn} after the switch has row {reading} at y = {y}, it was left at {before}");
    ensure!(ids == visible, "the same rows are on screen in the first frame");
    h.rest().await?;
    let settled = h.y_of(reading)?;
    ensure!(settled == before, "the position shifted after the first frame ({before} → {settled})");
    let restored = h.with(|list, cx| list.position(cx))?;
    ensure!(restored == saved, "the saved position round-trips: {saved:?} → {restored:?}");
    h.cx.log(format_args!("A → B → A: row {reading} at y = {before} px in the first frame and after; position {saved:?}"));
    Ok(())
}

async fn resize(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = resize_in(&h).await;
    h.close();
    result
}

async fn resize_in(h: &Harness) -> Result<()> {
    h.rest().await?;
    h.read_above_bottom(1500.)?;
    h.rest().await?;
    // The anchor is the row the viewport's top edge is in.
    let (anchor, before, width) = h.with(|list, _| {
        let id = list.visible_ids()[0];
        (id, list.row_screen_y(id), f32::from(list.bounds().size.width))
    })?;
    let heights = h.with(|list, _| list.visible_ids().iter().filter_map(|id| list.row_height(*id)).sum::<f32>())?;
    for side in [260., 520., 0.] {
        h.cx.update(|_, cx| h.pane.update(cx, |pane, cx| pane.set_side(side, cx)))?;
        let frames = h.cx.frames()?;
        // The first frame at the new width already has the anchor where it was.
        for _ in 0..200 {
            h.cx.sleep(Duration::from_millis(2)).await;
            if h.cx.frames()?.since(frames).drawn >= 1 {
                break;
            }
        }
        let now = h.with(|list, _| (list.row_screen_y(anchor), f32::from(list.bounds().size.width)))?;
        ensure!(now.0 == before, "the anchor row moved on a width change to {} px: {before:?} → {:?}", now.1, now.0);
        h.rest().await?;
        ensure!(h.with(|list, _| list.row_screen_y(anchor))? == before, "the anchor row moved after the late layouts at side {side}");
        h.cx.log(format_args!("width {} px: row {anchor} stays at y = {before:?}", now.1));
    }
    let again = h.with(|list, _| (f32::from(list.bounds().size.width), list.visible_ids().iter().filter_map(|id| list.row_height(*id)).sum::<f32>()))?;
    ensure!(again == (width, heights), "back at the first width the rows have their first heights again");
    // At the bottom a width change stays at the bottom.
    h.with(|list, cx| list.jump_to_present(cx))?;
    h.rest().await?;
    h.cx.update(|_, cx| h.pane.update(cx, |pane, cx| pane.set_side(300., cx)))?;
    h.rest().await?;
    ensure!(h.with(|list, cx| list.scroll_position(cx) == list.scroll_range().1)?, "a list at the bottom stays there when its width changes");
    Ok(())
}

/// What Chromium's selection copies for whole rows: every block of the row on its own line.
fn expected_copy(list: &MessageList, ids: &[i64], cx: &App) -> String {
    let format = decibell_model::Model::global(cx).settings.read(cx).time_format();
    let today = decibell_model::chat::time::local_day(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    let mut lines: Vec<String> = Vec::new();
    for id in ids {
        let Some((message, facts)) = list.row(*id) else { continue };
        if !facts.grouped {
            lines.push(decibell_common::display_name(Some(decibell_model::ServerId::from_central(1)), message.sender_uid, &message.sender, cx).to_string());
            lines.push(crate::row::format_timestamp(message.timestamp.unwrap_or(0), today, format));
        }
        if !message.content.is_empty() {
            let document = decibell_richtext::parse(&message.content, &decibell_richtext::ParseOptions::default());
            lines.push(decibell_richtext::plain::as_displayed(&document, &message.content));
        }
    }
    lines.join("\n")
}

async fn selection(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = selection_in(&h).await;
    h.close();
    result
}

async fn selection_in(h: &Harness) -> Result<()> {
    h.rest().await?;
    h.read_above_bottom(600.)?;
    h.rest().await?;
    let ids = h.with(|list, _| list.visible_ids())?;
    // Rows with text, fully on screen.
    let texts: Vec<i64> = h.with(|list, _| ids.iter().copied().filter(|id| list.body_point(*id, 0.5).is_some() && list.row_screen_y(*id).is_some_and(|y| y > 0.)).collect())?;
    ensure!(texts.len() >= 4, "the fixture shows several text rows");
    let (first, second, last) = (texts[0], texts[1], texts[texts.len() - 2]);

    // A drag from inside one body to inside another selects through the rows between.
    let from = h.with(|list, _| list.body_point(first, 0.25))?.context("no body")?;
    let to = h.with(|list, _| list.body_point(last, 0.75))?.context("no body")?;
    h.mouse_move(from, false)?;
    h.mouse_down(from, 1, false)?;
    h.mouse_move(point(from.x + px(30.), (from.y + to.y) / 2.), true)?;
    h.mouse_move(to, true)?;
    h.mouse_up(to)?;
    h.cx.settle().await?;
    let dragged = h.with(|list, cx| list.selected_text(cx))?.unwrap_or_default();
    let between: Vec<i64> = ids.iter().copied().skip_while(|id| *id != first).skip(1).take_while(|id| *id != last).collect();
    let middle = h.with(|list, cx| expected_copy(list, &between, cx))?;
    ensure!(!middle.is_empty() && dragged.contains(&middle), "a drag copies every row between its ends whole:\n{dragged:?}\nlacks\n{middle:?}");
    h.cx.log(format_args!("drag across {} rows copies {} chars; the rows between are whole", between.len() + 2, dragged.chars().count()));

    // Whole rows, selected by position: the text is exactly the rows as displayed.
    h.with(|list, cx| list.select((first, RowPos::START), (last, RowPos::END), cx))?;
    let whole: Vec<i64> = ids.iter().copied().skip_while(|id| *id != first).take_while(|id| *id != last).chain([last]).collect();
    let (copied, expected) = h.with(|list, cx| (list.selected_text(cx).unwrap_or_default(), expected_copy(list, &whole, cx)))?;
    ensure!(copied == expected, "whole rows copy as displayed:\n{copied:?}\nexpected\n{expected:?}");

    // Ctrl+C copies it (into a cell here: the test leaves the system clipboard alone).
    let copies = h.with(|list, _| list.capture_copies())?;
    h.with(|list, _| list.focus_handle().clone()).and_then(|focus| h.cx.update(|window, cx| focus.focus(window, cx)))?;
    h.key("ctrl-c")?;
    ensure!(copies.borrow().as_deref() == Some(expected.as_str()), "Ctrl+C copies the selection");

    // A click collapses; a double click takes a word, a triple click the paragraph.
    let at = h.with(|list, _| list.body_point(second, 0.5))?.context("no body")?;
    h.mouse_down(at, 1, false)?;
    h.mouse_up(at)?;
    ensure!(!h.with(|list, _| list.has_selection())?, "a click collapses the selection");
    h.mouse_down(at, 2, false)?;
    h.mouse_up(at)?;
    let word = h.with(|list, cx| list.selected_text(cx))?.unwrap_or_default();
    ensure!(!word.is_empty() && !word.contains(char::is_whitespace), "a double click selects one word: {word:?}");
    h.mouse_down(at, 3, false)?;
    h.mouse_up(at)?;
    let paragraph = h.with(|list, cx| list.selected_text(cx))?.unwrap_or_default();
    ensure!(paragraph.len() >= word.len() && paragraph.contains(&word), "a triple click selects the paragraph around the word");

    // Shift+click extends from the anchor.
    h.mouse_down(at, 1, false)?;
    h.mouse_up(at)?;
    h.mouse_down(to, 1, true)?;
    h.mouse_up(to)?;
    let extended = h.with(|list, cx| list.selected_text(cx))?.unwrap_or_default();
    ensure!(extended.contains('\n') && h.with(|list, _| list.has_selection())?, "Shift+click extends the selection across rows");

    // A drag held above the top edge scrolls, and the selection grows with it.
    let position = h.with(|list, cx| list.scroll_position(cx))?;
    let bounds = h.with(|list, _| list.bounds())?;
    h.mouse_down(at, 1, false)?;
    h.mouse_move(point(at.x, bounds.top() - px(40.)), true)?;
    h.cx.sleep(Duration::from_millis(400)).await;
    let (scrolled, grown) = h.with(|list, cx| (position - list.scroll_position(cx), list.selected_text(cx).unwrap_or_default().len()))?;
    h.cx.sleep(Duration::from_millis(200)).await;
    let further = h.with(|list, cx| position - list.scroll_position(cx))?;
    h.mouse_up(point(at.x, bounds.top() - px(40.)))?;
    ensure!(scrolled > 100. && further > scrolled, "a drag past the top edge keeps scrolling ({scrolled:.0} px, then {further:.0} px)");
    ensure!(grown > word.len(), "the selection grows while it autoscrolls");
    h.cx.log(format_args!("autoscroll 40 px past the edge: {scrolled:.0} px in 0.4 s, {further:.0} px in 0.6 s"));
    h.key("escape")?;
    h.rest().await?;

    // The scroll keys (docs/wheel-scroll.md §4).
    h.read_above_bottom(2000.)?;
    h.rest().await?;
    let height = f32::from(bounds.size.height) as f64;
    let before = h.with(|list, cx| list.scroll_position(cx))?;
    h.key("pagedown")?;
    h.rest().await?;
    let paged = h.with(|list, cx| list.scroll_position(cx))? - before;
    ensure!(paged == ScrollAnimator::page_step(height), "Page Down moves {} px, not {paged}", ScrollAnimator::page_step(height));
    h.key("down")?;
    h.rest().await?;
    ensure!(h.with(|list, cx| list.scroll_position(cx))? - before - paged == 40., "Arrow Down moves 40 px");
    h.key("up")?;
    h.key("space")?;
    h.rest().await?;
    ensure!(h.with(|list, cx| list.scroll_position(cx))? - before - paged == 40., "Arrow Up and Space are the composer's: the list does not scroll");
    h.key("end")?;
    h.rest().await?;
    ensure!(h.with(|list, _| list.at_bottom())?, "End goes to the bottom");
    h.key("home")?;
    h.cx.sleep(Duration::from_millis(250)).await;
    ensure!(!h.with(|list, _| list.at_bottom())?, "Home leaves the bottom");
    h.rest().await?;
    Ok(())
}

async fn scroll_spec(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = scroll_spec_in(&h).await;
    h.close();
    result
}

/// Feeds `inputs` (ms after the first, px) through the real element and compares every drawn
/// frame with the reference model (`ScrollAnimator`, which the kit checks against the spec's
/// vectors) fed the same inputs at the times the element saw them.
async fn follow(h: &Harness, name: &str, inputs: &[(u64, f64)], kind: ScrollKind) -> Result<()> {
    h.rest().await?;
    // Far from both edges, so no page arrives: 1200 px above the bottom of a long window.
    h.read_above_bottom(1200.)?;
    h.rest().await?;
    // Keys go to the list while it holds the focus (a click on it gives it).
    h.with(|list, _| list.focus_handle().clone()).and_then(|focus| h.cx.update(|window, cx| focus.focus(window, cx)))?;
    let scale = h.cx.update(|window, _| window.scale_factor())? as f64;
    let (min, max) = h.with(|list, _| list.scroll_range())?;
    let start = h.with(|list, cx| list.scroll_position(cx))?;
    let mut reference = ScrollAnimator::new();
    reference.set_max_offset(max - min);
    reference.jump_to(start - min);
    let began = Instant::now();
    let mut next = 0;
    let (mut frames, mut worst) = (0u32, 0f64);
    let mut seen = None;
    while began.elapsed() < Duration::from_millis(inputs.last().map_or(0, |last| last.0) + 450) {
        if next < inputs.len() && began.elapsed() >= Duration::from_millis(inputs[next].0) {
            let delta = inputs[next].1;
            match kind {
                // One notch is 3 GPUI lines (120 px); + is towards the top.
                ScrollKind::Wheel => h.wheel(-(delta / 40.) as f32)?,
                ScrollKind::Key => h.key(if delta > 100. { "pagedown" } else { "down" })?,
            }
            // The reference gets the input at the instant the element gave it to its animator.
            let at = h.with(|list, _| list.last_input())?.context("the element saw no input")?;
            reference.input(at, delta, kind);
            next += 1;
        }
        h.cx.sleep(Duration::from_millis(2)).await;
        let ((drawn, at), exact) = h.with(|list, _| (list.drawn(), list.drawn_exact()))?;
        if let Some(at) = at.filter(|at| seen != Some(*at)) {
            seen = Some(at);
            let model = min + reference.position(at);
            // The frame's position is the model's at the frame's time (the element saw each
            // input a few µs after the reference: hence a tolerance at all), drawn on a whole
            // device pixel.
            worst = worst.max((exact - model).abs());
            ensure!((drawn - (exact * scale).round() / scale).abs() < 1e-9, "{name}: a frame was not drawn on a device pixel");
            frames += 1;
        }
    }
    h.rest().await?;
    let total: f64 = inputs.iter().map(|input| input.1).sum();
    let travelled = h.with(|list, cx| list.scroll_position(cx))? - start;
    ensure!(travelled == total, "{name}: travelled {travelled} px, the inputs add up to {total}");
    ensure!(worst <= 0.001, "{name}: a frame was {worst} px off the model");
    ensure!(frames >= 10, "{name}: only {frames} frames were drawn");
    h.cx.log(format_args!("{name}: {frames} frames on the model's curve (worst {worst:.3} px), {travelled} px in all"));
    Ok(())
}

async fn scroll_spec_in(h: &Harness) -> Result<()> {
    follow(h, "W1 one notch down", &[(0, 120.)], ScrollKind::Wheel).await?;
    follow(h, "W2 one notch up", &[(0, -120.)], ScrollKind::Wheel).await?;
    follow(h, "W3 two notches 60 ms apart", &[(0, 120.), (60, 120.)], ScrollKind::Wheel).await?;
    follow(h, "W4 three notches 30 ms apart", &[(0, 120.), (30, 120.), (60, 120.)], ScrollKind::Wheel).await?;
    follow(h, "W6 reversal", &[(0, 120.), (60, -120.)], ScrollKind::Wheel).await?;
    follow(h, "K1 ArrowDown", &[(0, 40.)], ScrollKind::Key).await?;
    let page = h.with(|list, _| ScrollAnimator::page_step(f32::from(list.bounds().size.height) as f64))?;
    follow(h, "K2 PageDown", &[(0, page)], ScrollKind::Key).await?;
    // A precise delta (a touchpad) is applied as it is, in one frame.
    let before = h.with(|list, cx| list.scroll_position(cx))?;
    let position = h.center()?;
    h.input(PlatformInput::Scroll(ScrollEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.), px(37.))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: Default::default(),
    }))?;
    ensure!(h.with(|list, cx| (list.scroll_position(cx), list.is_animating()))? == (before - 37., false), "a precise delta moves at once, unanimated");
    Ok(())
}

async fn idle(cx: TestCx) -> Result<()> {
    let h = Harness::open(&cx, Which::General)?;
    let result = idle_in(&h).await;
    h.close();
    result
}

async fn idle_in(h: &Harness) -> Result<()> {
    let center = h.center()?;
    // The pointer rests over a row (its hover state is settled too).
    h.mouse_move(center, false)?;
    h.rest().await?;
    // The fixture's model has no core: its requests for the rows' senders (roster lookups,
    // pictures) fail and are given up on within a few seconds, each end a notify. Past those.
    h.cx.sleep(Duration::from_secs(6)).await;
    // The desktop is shared: another window opening or the real pointer crossing ours redraws
    // the title bar. Up to three windows of 10 s; one of them must be silent (the kit's
    // `assert_idle` does the same).
    let mut during = Frames { drawn: u64::MAX, presented: u64::MAX };
    for attempt in 1..=3 {
        let before = h.rest().await?;
        h.cx.sleep(Duration::from_secs(10)).await;
        during = h.cx.frames()?.since(before);
        if during.drawn == 0 && during.presented == 0 {
            break;
        }
        h.cx.log(format_args!("attempt {attempt}: {} drawn, {} presented in 10 s", during.drawn, during.presented));
    }
    ensure!(during.drawn == 0 && during.presented == 0, "an open channel at rest drew {} and presented {} frames in 10 s", during.drawn, during.presented);
    // A pointer move inside the same row costs nothing; into another row, one frame.
    let frames = h.cx.frames()?;
    for step in 0..20 {
        h.mouse_move(point(center.x + px(step as f32 * 3.), center.y), false)?;
    }
    h.cx.sleep(Duration::from_millis(100)).await;
    let same_row = h.cx.frames()?.since(frames).drawn;
    let frames = h.cx.frames()?;
    let started = Instant::now();
    for step in 0..40 {
        h.mouse_move(point(center.x, center.y - px(150.) + px(step as f32 * 7.5)), false)?;
    }
    let per_move = started.elapsed().as_secs_f64() * 1e6 / 40.;
    h.cx.sleep(Duration::from_millis(150)).await;
    let across = h.cx.frames()?.since(frames).drawn;
    h.cx.log(format_args!("0 frames in 10 s; 20 moves inside one target: {same_row} frames; 40 moves across 300 px of rows: {across} frames, {per_move:.1} µs per move"));
    ensure!(same_row <= 2, "moving inside one row's text redraws nothing ({same_row} frames)");
    Ok(())
}

/// Dumps (id, top, height) of every row of both fixture channels as the list lays them out in a
/// 1280 px window: `tools/electron-captures/chat-rows.mjs` compares them with the Electron
/// client's `offsetTop`s for the same fixture.
async fn rows(cx: TestCx) -> Result<()> {
    for (name, which) in [("chat-fixture", Which::General), ("chat-features", Which::Features)] {
        let h = Harness::open_pages(cx.app(), which, 3)?;
        let dump = async {
            h.rest().await?;
            let (width, height) = h.with(|list, _| (f32::from(list.bounds().size.width), f32::from(list.bounds().size.height)))?;
            let rows = h.with(|list, cx| list.row_tops(cx))?;
            anyhow::Ok((width, height, rows))
        }
        .await;
        h.close();
        let (width, height, rows) = dump?;
        let total: f64 = rows.iter().map(|row| row.2 as f64).sum();
        cx.log(format_args!("{name}: {} rows, {total} px, list {width} × {height}", rows.len()));
        ensure!(!rows.is_empty(), "{name} has rows");
        if let Some(dir) = std::env::var_os("DECIBELL_CHAT_ROWS_DIR") {
            let json = serde_json::json!({
                "list": { "width": width, "height": height },
                "rows": rows.iter().map(|(id, top, height)| serde_json::json!({ "id": id, "top": top, "height": height })).collect::<Vec<_>>(),
            });
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}-rows.json")), serde_json::to_vec_pretty(&json)?)?;
        }
    }
    Ok(())
}
