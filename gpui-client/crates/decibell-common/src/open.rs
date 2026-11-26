//! Opening the user menu and the profile popup (`uiStore.openContextMenu` /
//! `openProfilePopup`). They only set the request in the model (`Overlays`); the view that draws
//! the menu or the popup observes `model.overlays` and is a later feature's.
//!
//! Positions are window coordinates, as the renderer's: a right click opens the menu at the
//! pointer; a row click opens the profile 8 px right of the row's right edge, level with its top.

use decibell_model::stores::overlays::Anchor;
use decibell_model::{Model, ServerId, Uid};
use gpui::{App, Bounds, Pixels, Point};

/// The user context menu for `uid` at `at` (a right click), in the community it was opened in.
pub fn open_user_menu(uid: Uid, at: Point<Pixels>, server: Option<ServerId>, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let anchor = Anchor { x: f32::from(at.x), y: f32::from(at.y) };
    model.overlays.update(cx, |o, cx| o.open_context_menu(uid, anchor, server, cx));
}

/// The profile popup for `uid` beside a clicked row: `{ x: rect.right + 8, y: rect.top }`.
pub fn open_profile_beside(uid: Uid, row: Bounds<Pixels>, server: Option<ServerId>, cx: &mut App) {
    let at = Point { x: row.right() + gpui::px(8.), y: row.top() };
    open_profile_at(uid, at, server, cx);
}

/// The profile popup for `uid` at a point.
pub fn open_profile_at(uid: Uid, at: Point<Pixels>, server: Option<ServerId>, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let anchor = Anchor { x: f32::from(at.x), y: f32::from(at.y) };
    model.overlays.update(cx, |o, cx| o.open_profile_popup(uid, anchor, server, cx));
}

/// Where an element was laid out in the last frame, for handlers that anchor a popup to it (a
/// row's click opening the profile beside the row). Put [`BoundsCell::probe`] inside the element.
#[derive(Clone, Default)]
pub struct BoundsCell(std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>);

impl BoundsCell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Bounds<Pixels> {
        self.0.get()
    }

    /// An invisible child filling its (positioned) parent that records the parent's bounds when
    /// laid out. The parent must be `relative()`.
    pub fn probe(&self) -> impl gpui::IntoElement + use<> {
        use gpui::Styled as _;
        let cell = self.0.clone();
        gpui::canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().size_full()
    }
}
