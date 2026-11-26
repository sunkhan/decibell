//! What the two sidebars share (`ServerChannelsSidebar`, `ConversationSidebar`): the column
//! (`sidebar-above-panel relative flex shrink-0 flex-col border-r border-border bg-bg-dark`, the
//! width the user dragged it to) and its resize handle (`useSidebarResize`: 180–480 px, one width
//! for both sidebars, remembered per install in `ui.json`).

use decibell_model::Model;
use decibell_model::stores::layout::{SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN};
use crate::ink::Ink;
use gpui::{Context, DragMoveEvent, IntoElement, Pixels, Render, Window, div, prelude::*, px};

/// The drag of a sidebar's resize handle.
#[derive(Clone, Copy)]
pub struct SidebarResize;

/// What a drag shows under the pointer: nothing.
struct Nothing;

impl Render for Nothing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The width a sidebar draws at, following `Layout` and the handle's drag.
pub struct ResizeDrag {
    width: f32,
}

impl ResizeDrag {
    pub fn new(width: f32) -> Self {
        Self { width }
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    /// `Layout` changed: true when the width the sidebar draws at moved.
    pub fn width_changed(&mut self, width: f32) -> bool {
        std::mem::replace(&mut self.width, width) != width
    }

    /// The handle moved to `event`'s pointer: the sidebar's right edge follows it (the layout
    /// store clamps, keeps and writes it 200 ms after the last move).
    pub fn drag_to<V: 'static>(&mut self, event: &DragMoveEvent<SidebarResize>, _: &mut Window, cx: &mut Context<V>) {
        let width = f32::from(event.event.position.x - event.bounds.left())
            .round()
            .clamp(SIDEBAR_WIDTH_MIN as f32, SIDEBAR_WIDTH_MAX as f32);
        if width != self.width {
            self.width = width;
            let layout = Model::global(cx).layout.clone();
            layout.update(cx, |l, cx| l.set_sidebar_width(width, cx));
            cx.notify();
        }
    }
}

/// The sidebar column, `width` px wide, ending `padding_bottom` above the floating user panel.
pub fn sidebar_frame(id: &'static str, width: f32, padding_bottom: Pixels, t: &Ink) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .flex_col()
        .w(px(width))
        .h_full()
        .border_r_1()
        .border_color(t.color_border)
        .bg(t.color_bg_dark)
        .pb(padding_bottom)
}

/// The 4 px handle on the sidebar's right edge (`cursor-col-resize hover:bg-accent/40
/// active:bg-accent/60`). Its parent handles `on_drag_move::<SidebarResize>`.
pub fn resize_handle(id: &'static str, t: &Ink) -> impl IntoElement + use<> {
    let accent = t.color_accent;
    div()
        .id(id)
        .absolute()
        .right_0()
        .top_0()
        .h_full()
        .w_1()
        .cursor_col_resize()
        .hover(move |s| s.bg(accent.opacity(0.4)))
        .active(move |s| s.bg(accent.opacity(0.6)))
        .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| Nothing))
}
