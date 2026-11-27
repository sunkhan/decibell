//! The overlays that belong to the shell: the certificate-changed prompt
//! (`components/CertMismatchModal.tsx`, over the sign-in page too), the leave-server confirmation
//! (`components/LeaveServerConfirmModal.tsx`), the kicked / banned notice
//! (`features/servers/MembershipRevokedToast.tsx`), and the mirror of the model's toasts into
//! the kit's toast stack.

use std::collections::HashMap;

use decibell_common::Watch;
use decibell_model::regional::Regional;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::toasts::Severity;
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, CloseRequest, ConfirmModal, FontRole, Icon, IconName, Label, Modal,
    Toast, ToastId, Toasts,
};
use gpui::{App, ClickEvent, Context, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px};

use crate::requests::ShellRequests;

/// `sha256-hex` → `AB:CD:…` (`fingerprint.replace(/(.{2})(?=.)/g, "$1:").toUpperCase()`).
pub fn pretty_fingerprint(hex: &str) -> String {
    let mut out = String::with_capacity(hex.len() * 3 / 2);
    for (i, c) in hex.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.extend(c.to_uppercase());
    }
    out
}

/// "Certificate changed": refusing is the safe default; "Trust new certificate" re-pins and runs
/// what was refused again.
pub struct CertMismatchOverlay {
    busy: bool,
    _watch: Watch,
}

impl CertMismatchOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.overlays, cx);
        Self { busy: false, _watch: watch }
    }
}

impl Render for CertMismatchOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let notice = model.overlays.read(cx).cert_mismatch.clone();
        let open = notice.is_some();
        let t = crate::ink::base(cx);
        let shadow = cx.theme().shadow_modal.clone();
        let close = {
            let overlays = model.overlays.clone();
            move |cx: &mut App| overlays.update(cx, |o, cx| o.set_cert_mismatch(None, cx))
        };
        let card = notice.map(|notice| {
            let cert = notice.cert.clone();
            let retry = notice.retry.clone();
            let mono = cx.theme().font(FontRole::Mono);
            let _ = mono;
            div()
                .occlude()
                .w_full()
                .max_w(px(448.))
                .rounded(t.radius_xl)
                .border_1()
                .border_color(t.color_error.opacity(0.5))
                .bg(t.color_bg_secondary)
                .p_6()
                .shadow(shadow)
                .child(div().mb_2().child(Label::new("Certificate changed").font(FontRole::Display).size(16.).weight(600.).color(t.color_text_bright)))
                .child(
                    div()
                        .child(
                            Label::new(SharedString::from(format!("{}:{}", cert.host, cert.port)))
                                .font(FontRole::Mono)
                                .size(13.)
                                .color(t.color_text_primary),
                        )
                        .child(
                            Label::new(
                                "presented a TLS certificate that doesn't match the one this client trusts for it. That \
                                 happens when the operator re-generated their certificate — or when someone is intercepting \
                                 the connection.",
                            )
                            .size(13.)
                            .line_height(13. * 1.55)
                            .color(t.color_text_secondary),
                        ),
                )
                .child(div().mt_3().child(Label::new("PRESENTED FINGERPRINT (SHA-256)").size(11.).weight(600.).tracking(0.07).color(t.color_text_muted)))
                .child(
                    div().mt_1().child(
                        Label::new(SharedString::from(pretty_fingerprint(&cert.fingerprint)))
                            .font(FontRole::Mono)
                            .size(11.)
                            .color(t.color_text_secondary),
                    ),
                )
                .child(
                    div().mt_3().child(
                        Label::new("Only continue if the server operator confirms this fingerprint.")
                            .size(12.)
                            .line_height(18.)
                            .color(t.color_text_muted),
                    ),
                )
                .child(
                    div()
                        .mt_5()
                        .flex()
                        .gap_2p5()
                        .child(div().flex_1().child(Button::new("cert-cancel", "Cancel").variant(ButtonVariant::Secondary).full_width().probe("cert-cancel").on_click({
                            let close = close.clone();
                            move |_: &ClickEvent, _, cx| close(cx)
                        })))
                        .child(
                            div().flex_1().child(
                                Button::new("cert-trust", if self.busy { "Working…" } else { "Trust new certificate" })
                                    .variant(ButtonVariant::Danger)
                                    .full_width()
                                    .disabled(self.busy)
                                    .probe("cert-trust")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.busy = true;
                                        cx.notify();
                                        // `finally { setBusy(false) }`: a failure leaves the prompt open again.
                                        let view = cx.weak_entity();
                                        actions::servers::trust_certificate(cert.clone(), retry.clone(), cx, move |cx| {
                                            let _ = view.update(cx, |this, cx| {
                                                this.busy = false;
                                                cx.notify();
                                            });
                                        });
                                    })),
                            ),
                        ),
                )
        });
        if !open {
            self.busy = false;
        }
        Modal::new("cert-mismatch", open)
            // A press on the backdrop cancels; the renderer's prompt has no Escape.
            .on_close(move |request: &CloseRequest, _, cx| {
                if *request == CloseRequest::Backdrop {
                    close(cx)
                }
            })
            .children(card)
    }
}

/// "Leave <server>? You will need a new invite to rejoin."
pub struct LeaveServerOverlay {
    _watch: Watch,
}

impl LeaveServerOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.overlays, cx).store(&ShellRequests::entity(cx), cx);
        Self { _watch: watch }
    }
}

impl Render for LeaveServerOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let requests = ShellRequests::entity(cx);
        let pending = requests.read(cx).leave.clone();
        let open = model.overlays.read(cx).is_open(modal::LEAVE_SERVER_CONFIRM) && pending.is_some();
        let name = pending.as_ref().map(|(_, name)| name.clone()).unwrap_or_default();
        let close = {
            let overlays = model.overlays.clone();
            move |cx: &mut App| overlays.update(cx, |o, cx| o.close_modal(cx))
        };
        // `LeaveServerConfirmModal` is rendered inside the server sidebar: the chrome's palette
        // (console-split), and the name in `font-semibold text-text-primary` inside the
        // `mt-2 text-[13px] text-text-secondary` sentence.
        let chrome = &cx.theme().chrome;
        let (secondary, primary) = (chrome.color_text_secondary, chrome.color_text_primary);
        let sentence = decibell_ui::Runs::new(vec![
            ("Leave ".to_string(), decibell_ui::run_style(FontRole::Sans, 13., 400., secondary, cx)),
            (name.to_string(), decibell_ui::run_style(FontRole::Sans, 13., 600., primary, cx)),
            ("? You will need a new invite to rejoin.".to_string(), decibell_ui::run_style(FontRole::Sans, 13., 400., secondary, cx)),
        ]);
        let confirm = ConfirmModal::new("leave-server", open, "Leave server", "Leave")
            .child(div().mt_2().child(sentence))
            .on_cancel({
                let close = close.clone();
                move |_: &ClickEvent, _, cx| close(cx)
            })
            .on_confirm(move |_: &ClickEvent, _, cx| {
                let model = Model::global(cx).clone();
                let requests = ShellRequests::entity(cx);
                if let Some((server, _)) = requests.read(cx).leave.clone() {
                    actions::servers::leave(server, cx);
                    model.connections.update(cx, |c, cx| {
                        c.remove_connected(server, cx);
                        c.remove_pending(server, cx);
                    });
                    model.nav.update(cx, |nav, cx| {
                        nav.set_active_server(None, cx);
                        nav.set_active_channel(None, cx);
                        nav.set_active_view(ActiveView::Home, cx);
                    });
                    requests.update(cx, |r, cx| {
                        r.leave = None;
                        cx.notify();
                    });
                }
                close(cx);
            });
        decibell_ui::chrome_scope(confirm)
    }
}

/// "You have been banned from …": 8 s at the top of the window (the model clears it).
pub struct RevokedNoticeOverlay {
    _watch: Watch,
}

impl RevokedNoticeOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.overlays, cx);
        Self { _watch: watch }
    }
}

impl Render for RevokedNoticeOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let Some(notice) = model.overlays.read(cx).membership_revocation.clone() else { return div().into_any_element() };
        let t = crate::ink::base(cx);
        let shadow = cx.theme().shadow_modal.clone();
        let server_name = model
            .community(notice.server, cx)
            .map(|c| c.meta.read(cx).name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| model.servers.read(cx).get(notice.server).map(|s| s.name.clone()))
            .unwrap_or_else(|| notice.server.wire().into());
        let headline = decibell_model::live::revocation_headline(&notice.action, &server_name);
        let line = |text: String, color| div().mt_0p5().child(Label::new(SharedString::from(text)).size(12.).line_height(16.).color(color));
        let ban_line = (notice.action == "ban").then(|| {
            if notice.expires_at > 0 {
                // `formatDateTime` in the local zone and the user's clock: "Until Nov 7, 2026, 2:32 PM".
                let format = model.settings.read(cx).time_format();
                line(format!("Until {}", Regional::get().date_time(notice.expires_at, format)), t.color_text_muted)
            } else {
                line("This ban is permanent.".into(), t.color_text_muted)
            }
        });
        let overlays = model.overlays.clone();
        decibell_ui::layer(
            10,
            div().absolute().top_6().left_0().right_0().flex().justify_center().child(
                div()
                    .id("revoked-notice")
                    .occlude()
                    .flex()
                    .w_full()
                    .max_w(px(448.))
                    .items_start()
                    .gap_3()
                    .rounded(t.radius_lg)
                    .border_1()
                    .border_color(t.color_error.opacity(0.5))
                    .bg(t.color_bg_secondary)
                    .p_4()
                    .shadow(shadow)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .size_8()
                            .items_center()
                            .justify_center()
                            .rounded(t.radius_md)
                            .bg(t.color_error.opacity(0.15))
                            .child(Icon::new(IconName::AlertCircle).size(px(16.)).stroke_width(2.5).color(t.color_error)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(SharedString::from(headline)).size(14.).line_height(20.).weight(600.).color(t.color_text_bright))
                            .when(!notice.actor.is_empty(), |this| this.child(line(format!("By {}", notice.actor), t.color_text_secondary)))
                            .when(!notice.reason.is_empty(), |this| this.child(line(format!("Reason: {}", notice.reason), t.color_text_muted)))
                            .children(ban_line),
                    )
                    .child(
                        div()
                            .id("revoked-close")
                            .flex_none()
                            .cursor_pointer()
                            .on_click(move |_: &ClickEvent, _, cx| overlays.update(cx, |o, cx| o.set_membership_revocation(None, cx)))
                            .child(Icon::new(IconName::Close).size(px(14.)).color(t.color_text_muted)),
                    ),
            ),
        )
        .into_any_element()
    }
}

/// The model's toasts (pushed by event handlers) shown in the kit's toast stack: each new one is
/// pushed there with its lifetime and action, and taken down when the model drops it.
pub struct ToastMirror {
    shown: HashMap<u64, ToastId>,
    _subscription: Subscription,
}

impl ToastMirror {
    pub fn install(cx: &mut App) -> gpui::Entity<ToastMirror> {
        use gpui::AppContext as _;
        cx.new(|cx| {
            let model = Model::global(cx).clone();
            ToastMirror { shown: HashMap::new(), _subscription: cx.observe(&model.toasts, |this: &mut ToastMirror, toasts, cx| this.sync(&toasts, cx)) }
        })
    }

    fn sync(&mut self, toasts: &gpui::Entity<decibell_model::stores::toasts::Toasts>, cx: &mut Context<Self>) {
        let current: Vec<(u64, Toast)> = toasts
            .read(cx)
            .toasts()
            .iter()
            .filter(|t| !self.shown.contains_key(&t.id))
            .map(|t| {
                let mut toast = match t.severity {
                    Severity::Error => Toast::error(t.title.clone()),
                    Severity::Warning => Toast::warning(t.title.clone()),
                    Severity::Info => Toast::info(t.title.clone()),
                    Severity::Success => Toast::success(t.title.clone()),
                }
                .duration(t.duration);
                if let Some(body) = &t.body {
                    toast = toast.body(body.clone());
                }
                if let Some(action) = &t.action {
                    let run = action.run.clone();
                    toast = toast.action(action.label.clone(), move |_, cx| run(cx));
                }
                (t.id, toast)
            })
            .collect();
        let live: Vec<u64> = toasts.read(cx).toasts().iter().map(|t| t.id).collect();
        for (id, toast) in current {
            let shown = Toasts::push(cx, toast);
            self.shown.insert(id, shown);
        }
        let gone: Vec<u64> = self.shown.keys().copied().filter(|id| !live.contains(id)).collect();
        for id in gone {
            if let Some(shown) = self.shown.remove(&id) {
                Toasts::dismiss(cx, shown);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_read_like_the_renderer() {
        // A ban's end: `Regional::date_time` (decibell_model::regional's tests).
        assert_eq!(pretty_fingerprint("ab12cd"), "AB:12:CD");
        assert_eq!(pretty_fingerprint("abc"), "AB:C");
    }
}
