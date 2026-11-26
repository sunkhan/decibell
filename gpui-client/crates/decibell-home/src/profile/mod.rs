//! A person's profile: the anchored popout ([`ProfilePopup`], `UserProfilePopup.tsx`) and the
//! profile screen ([`ProfileModal`], `UserProfileModal.tsx`, Profile and Privacy tabs), with the
//! role chips both draw (`RoleChips.tsx`).

mod modal;
mod popup;

pub use modal::{ProfileModal, ProfileTab, open_profile};
pub use popup::ProfilePopup;

use decibell_common::role_color;
use decibell_model::stores::community::Role;
use decibell_ui::{ActiveTheme as _, Label};
use gpui::{App, Hsla, IntoElement, SharedString, div, prelude::*, px};

/// One chip: `inline-flex items-center gap-[5px] rounded-sm border border-border-divider
/// bg-bg-lighter px-2 py-[3px] text-[11px] font-medium text-text-secondary`, a 8 px dot.
fn chip(color: Hsla, label: SharedString, cx: &App) -> impl IntoElement + use<> {
    let t = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .rounded(t.radius_sm)
        .border_1()
        .border_color(t.color_border_divider)
        .bg(t.color_bg_lighter)
        .px_2()
        .py(px(3.))
        .child(div().size_2().rounded_full().bg(color))
        .child(Label::new(label).size(11.).weight(500.).color(t.color_text_secondary))
}

/// `RoleChips`: Owner first, then the member's roles in the order they hold them, or "Member"
/// when they hold none (or the server predates roles).
pub fn role_chips(roles: &[Role], role_ids: &[i64], is_owner: bool, cx: &App) -> impl IntoElement + use<> {
    let t = cx.theme();
    let (warning, muted, accent_bright) = (t.color_warning, t.color_text_muted, t.color_accent_bright);
    let held: Vec<&Role> = role_ids.iter().filter_map(|id| roles.iter().find(|r| r.id == *id)).collect();
    let mut chips = Vec::new();
    if is_owner {
        chips.push(chip(warning, "Owner".into(), cx).into_any_element());
    }
    for role in &held {
        chips.push(chip(role_color(role.color).unwrap_or(muted), role.name.clone(), cx).into_any_element());
    }
    if !is_owner && held.is_empty() {
        chips.push(chip(accent_bright, "Member".into(), cx).into_any_element());
    }
    div().flex().flex_wrap().gap_1p5().children(chips)
}
