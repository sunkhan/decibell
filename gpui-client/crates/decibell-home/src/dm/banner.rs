//! The strip under the DM header (`DmEncryptionBanner.tsx`) for the states that need attention,
//! in priority order: this device is locked (Unlock); the peer's keys changed (Dismiss); encryption
//! isn't set up yet (Not now / Set up); the peer has no keys. Nothing when everything is sealed or
//! central can't do encryption.

use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use decibell_model::{Model, Uid};
use decibell_ui::{ActiveTheme as _, Label};
use gpui::{AnyElement, App, ClickEvent, Hsla, IntoElement, SharedString, Window, div, prelude::*};

use crate::e2ee::PeerInfo;
use crate::ui::lock_glyph;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Warning,
    Info,
    Muted,
}

/// Which notice shows (the screens and tests read it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Locked,
    KeysChanged,
    SetUp,
    PeerHasNoKeys,
}

/// The notice for these facts (`DmEncryptionBanner`'s if / else chain).
pub fn notice(status: E2eeStatus, changed_at: i64, nudge_dismissed: bool, info: Option<&PeerInfo>) -> Option<Notice> {
    match status {
        E2eeStatus::Unavailable => None,
        E2eeStatus::Locked => Some(Notice::Locked),
        _ if changed_at > 0 => Some(Notice::KeysChanged),
        E2eeStatus::NotSetUp => (!nudge_dismissed).then_some(Notice::SetUp),
        E2eeStatus::Ready if info.is_some_and(|i| !i.has_keys) => Some(Notice::PeerHasNoKeys),
        _ => None,
    }
}

type Action = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `ActionButton`: primary `rounded-sm bg-accent px-3 py-1 text-[12px] font-semibold
/// text-on-accent`, else `rounded-sm border border-border px-3 py-1 text-[12px] font-medium
/// text-text-secondary hover:bg-surface-hover hover:text-text-primary`.
fn action_button(id: &'static str, label: &'static str, primary: bool, on_click: Action, cx: &App) -> AnyElement {
    let t = cx.theme();
    let (accent_hover, surface, text_primary) = (t.color_accent_hover, t.color_surface_hover, t.color_text_primary);
    div()
        .id(id)
        .flex()
        .flex_none()
        .rounded(t.radius_sm)
        .px_3()
        .py_1()
        .cursor_pointer()
        .map(|this| {
            if primary {
                this.bg(t.color_accent).text_color(t.color_on_accent).hover(move |s| s.bg(accent_hover))
            } else {
                this.border_1().border_color(t.color_border).text_color(t.color_text_secondary).hover(move |s| s.bg(surface).text_color(text_primary))
            }
        })
        .on_click(on_click)
        .child(Label::new(label).size(12.).weight(if primary { 600. } else { 500. }))
        .into_any_element()
}

pub fn banner(peer: Uid, name: &SharedString, info: Option<&PeerInfo>, cx: &App) -> Option<AnyElement> {
    let e2ee = Model::global(cx).live.e2ee.read(cx);
    let changed_at = e2ee.changed_peers.get(&peer).copied().unwrap_or(0);
    let notice = notice(e2ee.status, changed_at, e2ee.setup_nudge_dismissed, info)?;
    let open = |mode: PassphraseMode| -> Action {
        Box::new(move |_, _, cx| {
            let e2ee = Model::global(cx).live.e2ee.clone();
            e2ee.update(cx, |e, cx| e.open(mode, cx));
        })
    };
    let (tone, body, actions): (Tone, SharedString, Vec<AnyElement>) = match notice {
        Notice::Locked => (
            Tone::Warning,
            "Encryption is locked on this device. Enter your passphrase to read and send encrypted messages.".into(),
            vec![action_button("e2ee-unlock", "Unlock", true, open(PassphraseMode::Unlock), cx)],
        ),
        Notice::KeysChanged => (
            Tone::Warning,
            format!(
                "{name}'s encryption keys changed. If that's unexpected, compare safety numbers in their profile before trusting new messages."
            )
            .into(),
            vec![action_button(
                "e2ee-dismiss-change",
                "Dismiss",
                false,
                Box::new(move |_, _, cx| {
                    let e2ee = Model::global(cx).live.e2ee.clone();
                    e2ee.update(cx, |e, cx| e.dismiss_peer_change(peer, cx));
                }),
                cx,
            )],
        ),
        Notice::SetUp => (
            Tone::Info,
            format!("Set up end-to-end encryption so only you and {name} can read your messages, not even the server.").into(),
            vec![
                action_button(
                    "e2ee-not-now",
                    "Not now",
                    false,
                    Box::new(|_, _, cx| {
                        let e2ee = Model::global(cx).live.e2ee.clone();
                        e2ee.update(cx, |e, cx| e.dismiss_setup_nudge(cx));
                    }),
                    cx,
                ),
                action_button("e2ee-set-up", "Set up", true, open(PassphraseMode::Setup), cx),
            ],
        ),
        Notice::PeerHasNoKeys => {
            (Tone::Muted, format!("Messages with {name} aren't encrypted yet, they haven't set up encryption.").into(), Vec::new())
        }
    };
    let t = cx.theme();
    let (bg, fg, icon): (Hsla, Hsla, Hsla) = match tone {
        Tone::Warning => (t.color_warning.opacity(0.1), t.color_text_primary, t.color_warning),
        Tone::Info => (t.color_accent_soft, t.color_text_primary, t.color_accent),
        Tone::Muted => (t.color_bg_light, t.color_text_muted, t.color_text_muted),
    };
    Some(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(t.color_border)
            .px_4()
            .py_2()
            .bg(bg)
            .child(div().flex_none().child(lock_glyph(14., icon)))
            .child(div().flex_1().min_w_0().child(Label::new(body).size(12.).line_height(12. * 1.45).color(fg)))
            .when(!actions.is_empty(), |this| this.child(div().flex().flex_none().items_center().gap_1p5().children(actions)))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(keys: bool) -> PeerInfo {
        PeerInfo { has_keys: keys, fingerprint: "".into(), safety_number: "".into(), changed_at: 0, pinned: false }
    }

    #[test]
    fn the_notices_in_the_renderers_order() {
        assert_eq!(notice(E2eeStatus::Unavailable, 5, false, None), None);
        // Locked wins over a key change.
        assert_eq!(notice(E2eeStatus::Locked, 5, false, None), Some(Notice::Locked));
        assert_eq!(notice(E2eeStatus::Ready, 5, false, Some(&info(true))), Some(Notice::KeysChanged));
        assert_eq!(notice(E2eeStatus::NotSetUp, 0, false, None), Some(Notice::SetUp));
        assert_eq!(notice(E2eeStatus::NotSetUp, 0, true, None), None, "the nudge, dismissed");
        assert_eq!(notice(E2eeStatus::Ready, 0, false, Some(&info(false))), Some(Notice::PeerHasNoKeys));
        assert_eq!(notice(E2eeStatus::Ready, 0, false, Some(&info(true))), None, "sealed: nothing");
        assert_eq!(notice(E2eeStatus::Ready, 0, false, None), None, "still looking the peer up");
    }
}
