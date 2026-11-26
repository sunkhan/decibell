//! The confirm-then-join dialog for an invite that arrived as a deep link or a clicked chat link
//! (`features/servers/DeepLinkJoinModal.tsx`). Links carry only the code, so the endpoint comes
//! from central (`InviteResolve` → `resolve_invite_code`) with the community's name and
//! description for the preview; an older `host:port` link can still join while central is
//! unreachable. Open while `ServersStore::pending_invite` holds an invite: one set before sign-in
//! (a link that started the app) shows as soon as the main layout does.

use decibell_common::Watch;
use decibell_core::api;
use decibell_model::call::offer_retrust;
use decibell_model::error::UiError;
use decibell_model::regional::Regional;
use decibell_model::stores::invites::InviteChanged;
use decibell_model::stores::servers::PendingInvite;
use decibell_model::{Model, ServerId, actions};
use decibell_ui::{ActiveTheme as _, CloseRequest, FontRole, Label, Modal};
use gpui::{App, ClickEvent, Context, IntoElement, Render, SharedString, Task, Window, div, prelude::*, px};

use crate::ui::HEADING_TRACKING;

/// What the dialog shows, derived from the stores (the renderer's locals).
#[derive(Clone, Default)]
pub(crate) struct JoinState {
    pub code: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub resolving: bool,
    pub invalid: bool,
    pub resolve_error: Option<SharedString>,
    pub name: Option<SharedString>,
    pub description: Option<SharedString>,
    pub member_count: i32,
}

impl JoinState {
    fn server_id(&self) -> Option<ServerId> {
        (!self.host.is_empty() && self.port != 0).then(|| ServerId::from_wire(&format!("{}:{}", self.host, self.port)))
    }

    fn server_key(&self) -> String {
        if self.host.is_empty() || self.port == 0 { String::new() } else { format!("{}:{}", self.host, self.port) }
    }
}

pub struct JoinOverlay {
    model: Model,
    pending: Option<PendingInvite>,
    joining: bool,
    local_error: Option<SharedString>,
    _join: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
    _watch: Watch,
}

impl JoinOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        let mut subscriptions: Vec<gpui::Subscription> = Vec::new();
        subscriptions.push(cx.observe(&model.servers, |this: &mut Self, servers, cx| {
            let next = servers.read(cx).pending_invite.clone();
            if next == this.pending {
                return;
            }
            this.pending = next;
            this.opened(cx);
            cx.notify();
        }));
        watch
            .event(&model.invites, cx, |this: &mut Self, e: &InviteChanged| {
                this.pending.as_ref().is_some_and(|p| p.code.to_uppercase() == e.0.as_ref())
            })
            .store_if(&model.overlays, cx, |this: &mut Self, overlays, cx| {
                // An authentication failure for this invite's community stops the spinner; the
                // message shows inline.
                let state = this.state(cx);
                let Some(server) = state.server_id() else { return false };
                let failed = overlays.auth_error.as_ref().is_some_and(|e| e.server == server);
                if failed && this.joining {
                    this.joining = false;
                }
                this.pending.is_some()
            });
        let pending = model.servers.read(cx).pending_invite.clone();
        let mut this = Self { model, pending, joining: false, local_error: None, _join: None, _subscriptions: subscriptions, _watch: watch };
        this.opened(cx);
        cx.set_global(crate::Current::<JoinOverlay>(cx.weak_entity()));
        this
    }

    /// A new invite to show: look its code up, clear what the last attempt left.
    fn opened(&mut self, cx: &mut App) {
        self.joining = false;
        self._join = None;
        let Some(pending) = self.pending.clone() else { return };
        self.local_error = None;
        let code = pending.code.to_uppercase();
        let model = self.model.clone();
        // `useEffect`s: the request when the code changes, the stale error cleared on open.
        cx.defer(move |cx| {
            model.invites.update(cx, |i, cx| i.request(&code, cx));
            model.overlays.update(cx, |o, cx| o.set_auth_error(None, cx));
        });
    }

    pub(crate) fn state(&self, cx: &App) -> JoinState {
        let Some(pending) = &self.pending else { return JoinState::default() };
        let code = pending.code.to_uppercase();
        let entry = self.model.invites.read(cx).get(&code).cloned();
        let done = entry.as_ref().is_some_and(|e| !e.loading);
        let resolved = entry.as_ref().filter(|_| done).and_then(|e| e.resolved.clone());
        let host = pending.host.clone().or_else(|| resolved.as_ref().map(|r| r.host.clone())).unwrap_or_default();
        let port = pending.port.or_else(|| resolved.as_ref().map(|r| r.port)).unwrap_or(0);
        let invalid = done && entry.as_ref().is_some_and(|e| e.invalid);
        JoinState {
            code: pending.code.clone(),
            host,
            port,
            resolving: entry.as_ref().is_none_or(|e| e.loading),
            invalid,
            resolve_error: entry
                .as_ref()
                .filter(|e| done && e.resolved.is_none() && !e.invalid)
                .and_then(|e| e.error.clone()),
            name: resolved.as_ref().map(|r| r.server_name.clone()).filter(|n| !n.is_empty()),
            description: resolved.as_ref().map(|r| r.server_description.clone()).filter(|d| !d.is_empty()),
            member_count: resolved.as_ref().map_or(0, |r| r.member_count),
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let model = self.model.clone();
        model.servers.update(cx, |s, cx| s.set_pending_invite(None, cx));
        model.overlays.update(cx, |o, cx| o.set_auth_error(None, cx));
        self.local_error = None;
        cx.notify();
    }

    /// `handleJoin`.
    fn join(&mut self, cx: &mut Context<Self>) {
        let state = self.state(cx);
        let Some(server) = state.server_id() else { return };
        let Some(pending) = self.pending.clone() else { return };
        self.joining = true;
        self.local_error = None;
        self.model.overlays.update(cx, |o, cx| o.set_auth_error(None, cx));
        cx.notify();
        let args = api::servers::RedeemInviteArgs {
            server_id: server.wire(),
            host: state.host.to_string(),
            port: state.port as i32,
            invite_code: pending.code.to_string(),
        };
        self._join = Some(cx.spawn(async move |this, cx| {
            let result = if decibell_model::core::is_running() {
                api::servers::redeem_invite(args).await.map_err(UiError::from)
            } else {
                Err(UiError::not_running())
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    actions::servers::open_server(server, cx);
                    this.model.servers.update(cx, |s, cx| s.set_pending_invite(None, cx));
                }
                Err(e) => {
                    // `handleCertMismatch(err, handleJoin)`: trusting the new certificate joins again.
                    let weak = cx.weak_entity();
                    let retry: decibell_model::stores::overlays::Retry = std::rc::Rc::new(move |cx: &mut App| {
                        let _ = weak.update(cx, |this, cx| this.join(cx));
                    });
                    if !offer_retrust(&e, Some(retry), cx) {
                        this.local_error = Some(e.js_string());
                    }
                    this.joining = false;
                    cx.notify();
                }
            });
        }));
    }
}

impl Render for JoinOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.pending.is_some();
        let state = self.state(cx);
        let card = open.then(|| self.card(&state, cx));
        Modal::new("deep-link-join", open)
            // No Escape (no `useEscapeToClose`); a press on the backdrop closes.
            .on_close(cx.listener(|this, request: &CloseRequest, _, cx| {
                if *request == CloseRequest::Backdrop {
                    this.close(cx);
                }
            }))
            .children(card)
    }
}

impl JoinOverlay {
    fn card(&self, state: &JoinState, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.theme();
        let key = state.server_key();
        let server = state.server_id();
        let can_join = server.is_some() && !self.joining && !state.invalid;
        let title: SharedString = state.name.clone().unwrap_or_else(|| {
            if !key.is_empty() {
                key.clone().into()
            } else if state.resolving {
                "Resolving invite…".into()
            } else {
                "Couldn't resolve invite".into()
            }
        });
        let inline = self.model.overlays.read(cx).auth_error.clone().filter(|e| server.is_some_and(|s| e.server == s));
        let error_text: Option<SharedString> = match inline {
            Some(e) => Some(e.message.clone()),
            None => self
                .local_error
                .clone()
                .or_else(|| state.invalid.then(|| "This invite is unknown, expired, or used up.".into()))
                .or_else(|| state.resolve_error.clone().filter(|_| key.is_empty())),
        }
        .filter(|e| !e.is_empty());
        let members = if state.member_count > 0 {
            format!("{} members · ", Regional::get().group(state.member_count as u64))
        } else {
            String::new()
        };
        let accept_label = if self.joining {
            "Joining..."
        } else if state.resolving && key.is_empty() {
            "Resolving…"
        } else {
            "Accept"
        };
        let (accent, accent_hover, on_accent, surface_hover, primary, muted) =
            (t.color_accent, t.color_accent_hover, t.color_on_accent, t.color_surface_hover, t.color_text_primary, t.color_text_muted);
        div()
            .occlude()
            .w_full()
            .max_w(px(384.))
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_secondary)
            .p_6()
            .shadow(t.shadow_modal.clone())
            .child(
                div()
                    .mb_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new("Join server").size(18.).line_height(28.).weight(600.).tracking(HEADING_TRACKING).color(t.color_text_bright))
                    .child(
                        div()
                            .id("join-close")
                            .group("join-close")
                            .flex()
                            .size_8()
                            .items_center()
                            .justify_center()
                            .rounded(t.radius_md)
                            .cursor_pointer()
                            .hover(move |s| s.bg(surface_hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                            .child(
                                div()
                                    .text_color(muted)
                                    .group_hover("join-close", move |s| s.text_color(primary))
                                    .child(Label::new("×").size(16.).line_height(24.)),
                            ),
                    ),
            )
            .child(div().mb_2().child(Label::new("You have been invited to:").size(14.).line_height(20.).color(t.color_text_secondary)))
            .child(
                div()
                    .mb_1()
                    .rounded(t.radius_lg)
                    .border_1()
                    .border_color(t.color_border)
                    .bg(t.color_bg_dark)
                    .px_3()
                    .py_2p5()
                    .child(
                        Label::new(if state.invalid { SharedString::new_static("Invalid invite") } else { title })
                            .size(14.)
                            .line_height(20.)
                            .weight(600.)
                            .color(if state.invalid { t.color_text_muted } else { t.color_text_bright })
                            .truncate(),
                    )
                    .children(state.description.clone().filter(|_| !state.invalid).map(|d| {
                        div().mt_0p5().child(Label::new(d).size(12.).line_height(16.).color(t.color_text_secondary).clamp(2))
                    }))
                    .child(
                        div().mt_1().child(
                            Label::new(SharedString::from(format!("{members}Code: {}", state.code)))
                                .font(FontRole::Mono)
                                .size(11.)
                                .line_height(16.5)
                                .tracking(0.07)
                                .color(t.color_text_muted),
                        ),
                    ),
            )
            .children(error_text.map(|e| div().mt_3().child(Label::new(e).size(12.).line_height(16.).color(t.color_error))))
            .child(
                div()
                    .mt_4()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("join-cancel")
                            .flex_1()
                            .flex()
                            .justify_center()
                            .items_center()
                            .rounded(t.radius_md)
                            .border_1()
                            .border_color(t.color_border)
                            .bg(t.color_bg_primary)
                            .px_4()
                            .py_2()
                            .cursor_pointer()
                            .hover(move |s| s.bg(surface_hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                            .child(Label::new("Cancel").size(14.).line_height(20.).weight(600.).color(primary)),
                    )
                    .child(
                        div()
                            .id("join-accept")
                            .relative()
                            .children(decibell_shell::probes::probe("join-accept"))
                            .flex_1()
                            .flex()
                            .justify_center()
                            .rounded(t.radius_md)
                            .bg(accent)
                            .px_4()
                            .py_2()
                            // Stretched to the bordered Cancel's 38 px, the label centred.
                            .items_center()
                            .when(can_join, |this| this.cursor_pointer().hover(move |s| s.bg(accent_hover)))
                            .when(!can_join, |this| this.opacity(0.5))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if can_join {
                                    this.join(cx)
                                }
                            }))
                            .child(Label::new(accept_label).size(14.).line_height(20.).weight(600.).color(on_accent)),
                    ),
            )
    }
}
