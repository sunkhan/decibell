//! GPUI patch `0014` (`patches/README.md`): a clean cached view nested in a cached view that
//! renders again is replayed from its own cache instead of rendering with its parent.
//!
//! What that depends on, and what each test here holds it to:
//!
//! - **The ranges a view replays are indices into the previous frame.** A view that sits in a
//!   replayed ancestor is not visited, so its ranges have to be moved to where its content
//!   landed; the test that changes what is drawn *before* a replayed ancestor, and then renders
//!   the ancestor with the nested views clean, fails with wrong hit tests, lost actions and
//!   missing paint if they are not (that is why the pin forced the whole subtree to render).
//! - **Everything a replay carries still works**: hitboxes and click listeners, the focused
//!   element's key context and actions, its input handler, a deferred (popover) child.
//! - **The cache key still decides**: a nested view whose bounds, content mask or inherited
//!   text style changed renders again; so does a nested view that is dirty itself, and
//!   everything on `Window::refresh`.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, Modifiers, ParentElement as _, Pixels, Point, Render, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, TestAppContext, UTF16Selection, VisualTestContext, Window, actions, canvas, deferred, div,
    point, px, rgb,
};

actions!(nested_cached_views, [Poke]);

#[derive(Default)]
struct Counters {
    renders: Cell<usize>,
    clicks: Cell<usize>,
    pokes: Cell<usize>,
    popover_clicks: Cell<usize>,
    typed: Cell<usize>,
}

/// The innermost view: a clickable, focusable box with text, an input handler and a popover.
struct Leaf {
    name: &'static str,
    focus: FocusHandle,
    counters: Rc<Counters>,
    popover: bool,
}

impl Render for Leaf {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let counters = self.counters.clone();
        counters.renders.set(counters.renders.get() + 1);
        let name = self.name;
        let entity = cx.entity();
        let focus = self.focus.clone();
        div()
            .id(name)
            .debug_selector(move || format!("leaf-{name}"))
            .size_full()
            .relative()
            .bg(rgb(0x334455))
            .track_focus(&self.focus)
            .key_context("Leaf")
            .on_action({
                let counters = counters.clone();
                move |_: &Poke, _, _| counters.pokes.set(counters.pokes.get() + 1)
            })
            .on_click({
                let counters = counters.clone();
                move |_, _, _| counters.clicks.set(counters.clicks.get() + 1)
            })
            .child(format!("leaf {name}"))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(self.popover.then_some(()), |this, _| {
                let counters = counters.clone();
                this.child(deferred(
                    div()
                        .id("popover")
                        .debug_selector(move || format!("popover-{name}"))
                        .absolute()
                        .top(px(60.))
                        .left(px(10.))
                        .w(px(40.))
                        .h(px(30.))
                        .occlude()
                        .bg(rgb(0xaa5500))
                        .on_click(move |_, _, _| counters.popover_clicks.set(counters.popover_clicks.get() + 1)),
                ))
            })
    }
}

impl EntityInputHandler for Leaf {
    fn text_for_range(&mut self, _: Range<usize>, _: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, _: &mut Context<Self>) {
        // Counted without a notify: typing must not be what renders the view again.
        self.counters.typed.set(self.counters.typed.get() + text.len());
    }
    fn replace_and_mark_text_in_range(&mut self, _: Option<Range<usize>>, _: &str, _: Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) {}
    fn bounds_for_range(&mut self, _: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

/// The middle view: a row of leaves, each drawn cached at a fixed size.
struct Row {
    leaves: Vec<Entity<Leaf>>,
    renders: Rc<Cell<usize>>,
    /// Space before the leaves (moves them).
    indent: f32,
    /// The row's own width when it clips its leaves.
    clip_width: Option<f32>,
    red_text: bool,
}

impl Render for Row {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div()
            .size_full()
            .flex()
            .when_some(self.clip_width, |this, width| this.w(px(width)).overflow_hidden())
            .when(self.red_text, |this| this.text_color(rgb(0xff0000)))
            .child(div().flex_none().w(px(self.indent)).h(px(10.)))
            .children(self.leaves.iter().map(|leaf| {
                leaf.clone().cached(StyleRefinement::default().flex_none().w(px(100.)).h(px(100.)))
            }))
    }
}

/// A view drawn before the row: its number of boxes moves every index of what follows.
struct Side {
    boxes: usize,
    renders: Rc<Cell<usize>>,
}

impl Render for Side {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().size_full().flex().flex_col().children((0..self.boxes).map(|i| {
            div()
                .id(("side-box", i))
                .debug_selector(move || format!("side-{i}"))
                .h(px(4.))
                .w_full()
                .bg(rgb(0x202020))
                .cursor_pointer()
                .on_click(|_, _, _| {})
                .child(format!("box {i}"))
        }))
    }
}

/// The window's root (never cached, as the app's): the side column, then the row.
struct Top {
    side: Entity<Side>,
    row: Entity<Row>,
}

impl Render for Top {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(self.side.clone().cached(StyleRefinement::default().flex_none().w(px(50.)).h(px(400.))))
            .child(self.row.clone().cached(StyleRefinement::default().flex_none().w(px(600.)).h(px(100.))))
    }
}

struct Fixture {
    side: Entity<Side>,
    row: Entity<Row>,
    leaves: Vec<Entity<Leaf>>,
    counters: Vec<Rc<Counters>>,
    row_renders: Rc<Cell<usize>>,
    side_renders: Rc<Cell<usize>>,
}

impl Fixture {
    fn leaf_renders(&self) -> Vec<usize> {
        self.counters.iter().map(|c| c.renders.get()).collect()
    }
}

fn fixture(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("p", Poke, Some("Leaf"))]));
    let counters: Vec<Rc<Counters>> = (0..3).map(|_| Rc::new(Counters::default())).collect();
    let row_renders = Rc::new(Cell::new(0));
    let side_renders = Rc::new(Cell::new(0));
    let mut parts = None;
    let (_top, cx) = cx.add_window_view(|_, cx| {
        let leaves: Vec<Entity<Leaf>> = ["a", "b", "c"]
            .into_iter()
            .zip(&counters)
            .map(|(name, counters)| {
                cx.new(|cx| Leaf { name, focus: cx.focus_handle(), counters: counters.clone(), popover: name == "b" })
            })
            .collect();
        let row = cx.new(|_| Row { leaves: leaves.clone(), renders: row_renders.clone(), indent: 0., clip_width: None, red_text: false });
        let side = cx.new(|_| Side { boxes: 2, renders: side_renders.clone() });
        parts = Some((side.clone(), row.clone(), leaves));
        Top { side, row }
    });
    let (side, row, leaves) = parts.unwrap();
    (Fixture { side, row, leaves, counters, row_renders, side_renders }, cx)
}

fn centre(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} was not painted")).center()
}

/// Everything a replayed leaf carries works: where it is painted, its click listener (the
/// hitbox and the mouse listener), the focused leaf's action and input handler, the popover.
fn assert_leaves_work(f: &Fixture, cx: &mut VisualTestContext, indent: f32, what: &str) {
    for (index, selector) in ["leaf-a", "leaf-b", "leaf-c"].into_iter().enumerate() {
        let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{what}: {selector} was not painted"));
        assert_eq!(bounds.origin, point(px(50. + indent + 100. * index as f32), px(0.)), "{what}: {selector}");
        let before: Vec<usize> = f.counters.iter().map(|c| c.clicks.get()).collect();
        // Off the popover (which covers part of leaf b).
        cx.simulate_click(bounds.origin + point(px(80.), px(20.)), Modifiers::none());
        let after: Vec<usize> = f.counters.iter().map(|c| c.clicks.get()).collect();
        let expected: Vec<usize> = before.iter().enumerate().map(|(i, n)| n + usize::from(i == index)).collect();
        assert_eq!(after, expected, "{what}: a click on {selector}");
    }
    // The focused leaf (c) takes the bound key and the typed text; nobody else does.
    let pokes: Vec<usize> = f.counters.iter().map(|c| c.pokes.get()).collect();
    cx.simulate_keystrokes("p");
    assert_eq!(
        f.counters.iter().map(|c| c.pokes.get()).collect::<Vec<_>>(),
        [pokes[0], pokes[1], pokes[2] + 1],
        "{what}: the focused leaf's action"
    );
    let typed = f.counters[2].typed.get();
    cx.simulate_input("xy");
    assert_eq!(f.counters[2].typed.get(), typed + 2, "{what}: the focused leaf's input handler");
    // The popover of leaf b is drawn over it and takes its own clicks.
    let popover = centre(cx, "popover-b");
    let (clicks, leaf_clicks) = (f.counters[1].popover_clicks.get(), f.counters[1].clicks.get());
    cx.simulate_click(popover, Modifiers::none());
    assert_eq!(
        (f.counters[1].popover_clicks.get(), f.counters[1].clicks.get()),
        (clicks + 1, leaf_clicks),
        "{what}: the popover"
    );
}

fn focus_last_leaf(f: &Fixture, cx: &mut VisualTestContext) {
    let focus = cx.update(|_, cx| f.leaves[2].read(cx).focus.clone());
    cx.update(|window, cx| focus.focus(window, cx));
    cx.run_until_parked();
}

/// Renders caused by `change` and the frame it leads to: (each leaf, the row, the side
/// column). Counted around the one frame: the checks between steps click and type, which
/// renders views for their own reasons (pressed states, the keyboard / pointer modality).
fn renders_of(f: &Fixture, cx: &mut VisualTestContext, change: impl FnOnce(&Fixture, &mut VisualTestContext)) -> ([usize; 3], usize, usize) {
    cx.run_until_parked();
    let (leaves, rows, sides) = (f.leaf_renders(), f.row_renders.get(), f.side_renders.get());
    change(f, cx);
    cx.run_until_parked();
    let now = f.leaf_renders();
    ([now[0] - leaves[0], now[1] - leaves[1], now[2] - leaves[2]], f.row_renders.get() - rows, f.side_renders.get() - sides)
}

fn notify_row(f: &Fixture, cx: &mut VisualTestContext) {
    f.row.update(cx, |_, cx| cx.notify());
}

fn set_side(boxes: usize) -> impl FnOnce(&Fixture, &mut VisualTestContext) {
    move |f, cx| {
        f.side.update(cx, |side, cx| {
            side.boxes = boxes;
            cx.notify();
        })
    }
}

/// The patch's point. Fails at the pin: every leaf rendered with the row.
#[gpui::test]
fn a_clean_view_nested_in_a_dirty_one_is_replayed(cx: &mut TestAppContext) {
    let (f, cx) = fixture(cx);
    focus_last_leaf(&f, cx);
    for round in 1..=3 {
        // The row is dirty: it renders. Its leaves are clean: none does. Nor does a view
        // beside it.
        assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0), "round {round}");
        assert_leaves_work(&f, cx, 0., "the row rendered again");
    }
    // A dirty leaf renders, with the row (GPUI marks a view's ancestors dirty with it), and
    // its siblings do not.
    let one_leaf = renders_of(&f, cx, |f, cx| f.leaves[1].update(cx, |_, cx| cx.notify()));
    assert_eq!(one_leaf, ([0, 1, 0], 1, 0));
    assert_leaves_work(&f, cx, 0., "one leaf rendered again");
}

/// The hazard: the row is replayed whole in a frame that draws something else before it (so
/// everything of the row lands at other indices), the leaves inside it are not visited, and
/// then the row renders with its leaves clean. Each leaf must replay what is its own.
#[gpui::test]
fn nested_views_follow_a_replayed_ancestor_to_where_it_landed(cx: &mut TestAppContext) {
    let (f, cx) = fixture(cx);
    focus_last_leaf(&f, cx);
    for (round, boxes) in [7usize, 1, 12, 13, 0, 5].into_iter().enumerate() {
        // The side column changes: more or fewer hitboxes, listeners, text and paint before
        // the row, which is replayed whole.
        assert_eq!(renders_of(&f, cx, set_side(boxes)), ([0, 0, 0], 0, 1), "round {round}: the row is clean");
        assert_leaves_work(&f, cx, 0., "the row was replayed whole");
        // Now and then a second frame in which the row is replayed, where it already is.
        if round % 2 == 1 {
            assert_eq!(renders_of(&f, cx, |f, cx| f.side.update(cx, |_, cx| cx.notify())), ([0, 0, 0], 0, 1));
        }
        // Now the row renders; its leaves are clean and replay from where they landed.
        assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0), "round {round}: the leaves are replayed");
        assert_leaves_work(&f, cx, 0., "the row rendered after being replayed elsewhere");
        // And the side column and the row in one frame.
        let both = renders_of(&f, cx, |f, cx| {
            set_side(boxes + 3)(f, cx);
            notify_row(f, cx);
        });
        assert_eq!(both, ([0, 0, 0], 1, 1), "round {round}: both at once");
        assert_leaves_work(&f, cx, 0., "the side column and the row rendered together");
    }
}

/// What the cache key holds: a nested view that the parent moved, clipped or gave another
/// text style renders again; one that the parent left as it was does not.
#[gpui::test]
fn a_nested_view_whose_place_or_inherited_style_changed_renders(cx: &mut TestAppContext) {
    let (f, cx) = fixture(cx);
    focus_last_leaf(&f, cx);
    let change_row = |change: fn(&mut Row)| {
        move |f: &Fixture, cx: &mut VisualTestContext| {
            f.row.update(cx, |row, cx| {
                change(row);
                cx.notify();
            })
        }
    };
    // Moved: every leaf's bounds changed.
    assert_eq!(renders_of(&f, cx, change_row(|row| row.indent = 20.)), ([1, 1, 1], 1, 0), "moved leaves render");
    assert_leaves_work(&f, cx, 20., "the leaves moved");
    // Not moved: replayed.
    assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0), "leaves the row left alone are replayed");
    // Clipped: the row is 250 px wide now and cuts its leaves with another content mask.
    assert_eq!(renders_of(&f, cx, change_row(|row| row.clip_width = Some(250.))), ([1, 1, 1], 1, 0), "a new content mask renders");
    // The mask moves again; the leaves' bounds do not.
    assert_eq!(renders_of(&f, cx, change_row(|row| row.clip_width = Some(300.))), ([1, 1, 1], 1, 0), "a changed content mask renders");
    assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0), "the same mask is replayed");
    assert_eq!(renders_of(&f, cx, change_row(|row| row.clip_width = None)), ([1, 1, 1], 1, 0));
    // The inherited text style.
    assert_eq!(renders_of(&f, cx, change_row(|row| row.red_text = true)), ([1, 1, 1], 1, 0), "an inherited text colour renders");
    assert_leaves_work(&f, cx, 20., "after the style change");
    // `Window::refresh` renders everything, as before.
    assert_eq!(renders_of(&f, cx, |_, cx| cx.update(|window, _| window.refresh())), ([1, 1, 1], 1, 1), "a refresh renders every view");
    // And the frame after it replays again.
    assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0));
    assert_leaves_work(&f, cx, 20., "after a refresh");
}

/// A leaf that left the tree for a frame has nothing to replay when it comes back.
#[gpui::test]
fn a_view_that_was_not_drawn_in_the_last_frame_renders(cx: &mut TestAppContext) {
    let (f, cx) = fixture(cx);
    let all = f.leaves.clone();
    let gone = renders_of(&f, cx, |f, cx| {
        f.row.update(cx, |row, cx| {
            row.leaves.truncate(2);
            cx.notify();
        })
    });
    assert_eq!(gone, ([0, 0, 0], 1, 0), "the two that stayed are replayed");
    assert_eq!(renders_of(&f, cx, notify_row), ([0, 0, 0], 1, 0));
    let back = renders_of(&f, cx, move |f, cx| {
        f.row.update(cx, |row, cx| {
            row.leaves = all;
            cx.notify();
        })
    });
    assert_eq!(back, ([0, 0, 1], 1, 0));
    assert!(cx.debug_bounds("leaf-c").is_some());
}
