//! Scrolling as Chromium scrolls on Linux (`gpui-client/docs/wheel-scroll.md`): the pure
//! [`ScrollAnimator`] (reused by the message list with its own element) and the [`ScrollArea`]
//! container with Chromium's scrollbar.

mod animator;
mod area;

pub use animator::{
    LINE_STEP_PX, PAGE_STEP_FRACTION, PX_PER_GPUI_LINE, ScrollAnimator, ScrollKind, WHEEL_NOTCH_PX,
};
pub use area::{RowHeights, ScrollAlign, ScrollArea, ScrollFrame, ScrollHandle};
