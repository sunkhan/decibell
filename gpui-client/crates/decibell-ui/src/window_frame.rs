//! What a window draws around its content when it has no server-side frame.
//!
//! The window is client-decorated on Linux (Wayland, and X11 under a compositing window
//! manager): GPUI then gives us a surface with nothing on it, and the shadow, the border, the
//! rounded top corners and the resize edges are ours to draw, in a transparent margin around
//! the visible window that the compositor is told about (`set_client_inset`). On Windows the
//! system keeps drawing the frame, shadow and resize borders around our title bar, and under a
//! server-decorating X11 window manager or compositor GPUI falls back to server decorations:
//! in both cases the content just fills the window.

use gpui::{
    AnyElement, App, BoxShadow, CursorStyle, Decorations, Global, Hsla, IntoElement, MouseButton,
    Pixels, ResizeEdge, Size, Tiling, Window, div, hsla, prelude::*, px, size,
};

use crate::ActiveTheme as _;

/// The transparent margin around a floating client-decorated window: room for the shadow, and
/// the band in which the pointer resizes the window.
const INSET: Pixels = px(10.);
/// How far the corner resize zones reach along each edge.
const CORNER: Pixels = px(24.);

/// The floating window's own shadow and outline. Not a theme token: it has to fade out inside
/// [`INSET`] (the surface ends there), which none of the elevation shadows does.
fn window_shadow(outline: Hsla) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(2.), hsla(0., 0., 0., 0.35)).blur_radius(px(3.)),
        BoxShadow::new(px(0.), px(0.), outline).spread_radius(px(1.)),
    ]
}

/// The size to open a window at so that its visible part is `visible`: on Linux the surface
/// of a client-decorated window also holds the margin.
pub fn outer_window_size(visible: Size<Pixels>) -> Size<Pixels> {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        size(visible.width + INSET * 2., visible.height + INSET * 2.)
    } else {
        visible
    }
}

/// Set as a global by `decibell-capture`: draw every window as a floating client-decorated
/// one, whatever the platform reports. (GPUI's headless test window reports server
/// decorations and cannot be told otherwise from outside the gpui crate.)
pub struct AssumeClientDecorations;

impl Global for AssumeClientDecorations {}

/// How the window is decorated. Frame code asks this, never the window directly.
pub fn decorations(window: &Window, cx: &App) -> Decorations {
    if cx.has_global::<AssumeClientDecorations>() {
        Decorations::Client {
            tiling: Tiling::default(),
        }
    } else {
        window.window_decorations()
    }
}

/// The radius of the window's top corners while it floats (`0` while tiled, maximised or
/// server-decorated). The bottom corners stay square: GPUI clips to rectangles only, so
/// content could not follow a rounded bottom edge.
pub fn window_corner_radius(window: &Window, cx: &App) -> Pixels {
    match decorations(window, cx) {
        Decorations::Client { tiling } if !tiling.is_tiled() => cx.theme().radius_lg.min(px(10.)),
        _ => px(0.),
    }
}

/// Wraps the root view's content (title bar and all) in the window frame.
pub fn window_frame(content: impl IntoElement, window: &mut Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Decorations::Client { tiling } = decorations(window, cx) else {
        return div()
            .size_full()
            .bg(theme.color_bg_primary)
            .child(content)
            .into_any_element();
    };
    // GPUI leaves the margin out on tiled edges by itself; our padding below matches it.
    window.set_client_inset(INSET);
    let radius = window_corner_radius(window, cx);

    div()
        .relative()
        .size_full()
        .when(!tiling.top, |frame| frame.pt(INSET))
        .when(!tiling.bottom, |frame| frame.pb(INSET))
        .when(!tiling.left, |frame| frame.pl(INSET))
        .when(!tiling.right, |frame| frame.pr(INSET))
        .child(
            div()
                .size_full()
                .bg(theme.color_bg_primary)
                .rounded_t(radius)
                .when(!tiling.is_tiled(), |window| {
                    window.shadow(window_shadow(theme.chrome.color_border))
                })
                .child(content),
        )
        .children(resize_handles(tiling))
        .into_any_element()
}

/// Invisible bands in the margin, each with its resize cursor: one per edge and an L of two
/// per corner. Plain elements with a cursor style: nothing is redrawn while the pointer moves
/// over them.
fn resize_handles(tiling: Tiling) -> impl Iterator<Item = AnyElement> {
    use ResizeEdge::*;
    // (edge, the band runs along the top or bottom side rather than the left or right)
    [
        (Top, true),
        (Bottom, true),
        (Left, false),
        (Right, false),
        (TopLeft, true),
        (TopLeft, false),
        (TopRight, true),
        (TopRight, false),
        (BottomLeft, true),
        (BottomLeft, false),
        (BottomRight, true),
        (BottomRight, false),
    ]
    .into_iter()
    .filter(move |(edge, _)| {
        let (top, bottom, left, right) = sides(*edge);
        !(top && tiling.top
            || bottom && tiling.bottom
            || left && tiling.left
            || right && tiling.right)
    })
    .map(|(edge, horizontal)| {
        let (top, bottom, left, right) = sides(edge);
        let corner = (top || bottom) && (left || right);
        let cursor = match edge {
            Top | Bottom => CursorStyle::ResizeUpDown,
            Left | Right => CursorStyle::ResizeLeftRight,
            TopLeft | BottomRight => CursorStyle::ResizeUpLeftDownRight,
            TopRight | BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        };
        div()
            .absolute()
            .cursor(cursor)
            .map(|handle| match (corner, horizontal) {
                (true, true) => handle.w(CORNER).h(INSET),
                (true, false) => handle.w(INSET).h(CORNER),
                (false, true) => handle.h(INSET).left(CORNER).right(CORNER),
                (false, false) => handle.w(INSET).top(CORNER).bottom(CORNER),
            })
            .when(top, |handle| handle.top_0())
            .when(bottom, |handle| handle.bottom_0())
            .when(left, |handle| handle.left_0())
            .when(right, |handle| handle.right_0())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
            .into_any_element()
    })
}

/// Which sides of the window an edge or corner touches: (top, bottom, left, right).
fn sides(edge: ResizeEdge) -> (bool, bool, bool, bool) {
    use ResizeEdge::*;
    (
        matches!(edge, Top | TopLeft | TopRight),
        matches!(edge, Bottom | BottomLeft | BottomRight),
        matches!(edge, Left | TopLeft | BottomLeft),
        matches!(edge, Right | TopRight | BottomRight),
    )
}
