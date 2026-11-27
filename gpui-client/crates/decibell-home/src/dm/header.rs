//! The conversation's header (`DmChatPanel.tsx`): the peer's avatar with its dot, name, status
//! pill, custom status, the encryption chip (`EncryptionBadge.tsx`), then the call button, "Show
//! chat" while the call stage fills the panel, search (inert, as in the renderer) and the friends
//! rail toggle.

use decibell_common::{StatusMessageText, UserAvatar, dot_presence};
use decibell_model::live::calls::CallStatus;
use decibell_model::live::e2ee::E2eeStatus;
use decibell_model::live::presence::{DisplayStatus, StatusMessage};
use decibell_model::{Model, Uid, actions};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, StatusDot, title_tooltip};
use gpui::{AnyElement, App, ClickEvent, ElementId, IntoElement, SharedString, Stateful, div, prelude::*, px};

use crate::e2ee::PeerInfo;
use crate::ui::{lock_glyph, status_pill};

/// What the header shows, read once per render of the conversation.
pub struct HeaderFacts {
    pub peer: Uid,
    pub name: SharedString,
    pub presence: DisplayStatus,
    pub message: Option<StatusMessage>,
    pub status: E2eeStatus,
    pub info: Option<PeerInfo>,
    pub call_status: CallStatus,
    pub call_signaling: bool,
    /// The call stage fills the panel; the count of messages since it did.
    pub theater: Option<usize>,
    pub rail_on: bool,
}

/// `EncryptionBadge`: nothing on a central without keys, nothing while the peer's state is being
/// looked up (no "Unencrypted" flash on open).
pub fn encryption_badge(f: &HeaderFacts, cx: &App) -> Option<AnyElement> {
    if f.status == E2eeStatus::Unavailable || (f.status == E2eeStatus::Ready && f.info.is_none()) {
        return None;
    }
    let on = f.status == E2eeStatus::Ready && f.info.as_ref().is_some_and(|i| i.has_keys);
    let title: SharedString = if on {
        "End-to-end encrypted. Only you and this person can read these messages".into()
    } else {
        match f.status {
            E2eeStatus::Locked => "Encryption is locked on this device. Enter your passphrase in Settings → Privacy".into(),
            E2eeStatus::NotSetUp => "Set up end-to-end encryption in Settings → Privacy".into(),
            _ => format!("{} hasn't set up encryption yet, messages are not encrypted", f.name).into(),
        }
    };
    let t = cx.theme();
    let color = if on { t.color_success } else { t.color_text_muted };
    Some(
        div()
            .id("dm-encryption-badge")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .rounded(t.radius_sm)
            .px_2()
            .py_0p5()
            .bg(color.opacity(0.15))
            .tooltip(title_tooltip(title))
            .child(if on {
                lock_glyph(11., color)
            } else {
                Icon::new(IconName::HomeLockOpen).size(px(11.)).color(color)
            })
            .child(Label::new(if on { "Encrypted" } else { "Unencrypted" }).font(FontRole::Channel).size(11.).weight(500.).color(color))
            .into_any_element(),
    )
}

/// One of the header's 32 px buttons: `h-8 w-8 rounded-sm text-text-muted hover:bg-surface-hover
/// hover:text-text-secondary`; disabled at 40 %, no hover.
pub fn header_button(id: impl Into<ElementId>, icon: IconName, title: impl Into<SharedString>, disabled: bool, cx: &App) -> Stateful<gpui::Div> {
    let t = cx.theme();
    let id: ElementId = id.into();
    let group: SharedString = format!("dm-header-{id}").into();
    let title: SharedString = title.into();
    let (hover, muted, secondary) = (t.color_surface_hover, t.color_text_muted, t.color_text_secondary);
    div()
        .id(id)
        .group(group.clone())
        .relative()
        .flex()
        .flex_none()
        .size_8()
        .items_center()
        .justify_center()
        .rounded(t.radius_sm)
        .when(!title.is_empty(), |this| this.tooltip(title_tooltip(title)))
        .map(|this| {
            if disabled {
                this.opacity(0.4).cursor_not_allowed().child(Icon::new(icon).size(px(18.)).color(muted))
            } else {
                this.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(Icon::new(icon).size(px(18.)).color(muted).group_hover(group, secondary))
            }
        })
}

pub fn header(f: &HeaderFacts, cx: &mut App) -> impl IntoElement + use<> {
    let peer = f.peer;
    let badge = encryption_badge(f, cx);
    let online = f.presence.is_online();
    let idle = f.call_status == CallStatus::Idle;
    // Away and Do Not Disturb can still be called; Invisible looks offline.
    let call_title: SharedString = if !f.call_signaling {
        "Calls aren't available on this server yet".into()
    } else if !online {
        format!("{} is offline", f.name).into()
    } else if !idle {
        "Already in a call".into()
    } else {
        "Start a voice call".into()
    };
    let call_disabled = !online || !f.call_signaling || !idle;
    let call = header_button("call", IconName::ShellPhone, call_title, call_disabled, cx)
        .children(decibell_shell::probes::probe("dm-call"))
        .when(!call_disabled, |this| this.on_click(move |_: &ClickEvent, _, cx| actions::calls::start_call(peer, cx)));
    let theater = f.theater.map(|unread| {
        let t = cx.theme();
        let (error, bright) = (t.color_error, t.color_text_bright);
        header_button("show-chat", IconName::SettingsMessage, "Show chat", false, cx)
            .on_click(|_: &ClickEvent, _, cx| {
                let calls = Model::global(cx).live.calls.clone();
                calls.update(cx, |c, cx| c.set_theater(false, 0, cx));
            })
            .when(unread > 0, |this| {
                this.child(
                    div()
                        .absolute()
                        .right_0p5()
                        .top_0p5()
                        .flex()
                        .h_4()
                        .min_w_4()
                        .px_1()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(error)
                        .child(
                            Label::new(SharedString::from(if unread > 99 { "99+".to_string() } else { unread.to_string() }))
                                .size(10.)
                                .weight(600.)
                                .color(bright),
                        ),
                )
            })
    });
    let search = header_button("search", IconName::HomeDmSearch, "", false, cx);
    // On looks the same as off, like the members toggle (owner, 2026-10-14).
    let rail = header_button("friends-rail", IconName::ShellPanel, if f.rail_on { "Hide friends" } else { "Show friends" }, false, cx)
        .on_click(|_: &ClickEvent, _, cx| {
            let layout = Model::global(cx).layout.clone();
            layout.update(cx, |l, cx| l.toggle_dm_friends_panel(cx));
        });
    let t = cx.theme();
    let (border, ring, bright) = (t.color_border, t.color_bg_tertiary, t.color_text_bright);
    let (title_size, weight, tracking) = (t.scaled.text_title, t.palette.font_weight_emphasis, t.palette.tracking_title);
    let muted = t.color_text_muted;
    div()
        .flex()
        .flex_none()
        .h_12()
        .items_center()
        .gap_2p5()
        .border_b_1()
        .border_color(border)
        .px_4()
        .child(
            div()
                .relative()
                .flex_none()
                .child(UserAvatar::new(peer, f.name.clone(), 26.))
                .child(div().absolute().bottom(px(-1.)).right(px(-1.)).child(StatusDot::new(dot_presence(f.presence), ring).size(12.).ring_width(2.))),
        )
        .child(div().flex_none().child(Label::new(f.name.clone()).font(FontRole::Display).size(title_size).weight(weight).tracking(tracking).color(bright).nowrap()))
        .child(status_pill(f.presence, FontRole::Channel, cx))
        .when_some(f.message.clone(), |this, message| {
            this.child(div().min_w_0().max_w(px(384.)).child(StatusMessageText::new(message).font(FontRole::Channel).size(13.).color(muted).emoji_size(16.)))
        })
        .children(badge)
        .child(div().flex_1())
        .child(div().flex().flex_none().gap_1().child(call).children(theater).child(search).child(rail))
}
