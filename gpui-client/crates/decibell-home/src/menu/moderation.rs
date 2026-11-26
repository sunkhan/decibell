//! The member dialogs the user menu opens (`MembersTab.tsx`): kick / ban / time out
//! (`ModerationDialog`: the person, what it does to them, a duration and a purge window for bans,
//! the durations for a timeout, the reason they and the audit log see) and the nickname (the
//! renderer edits it inline in the Members tab; here a small dialog with the same field: the
//! server's 32-byte cap as typed, the member's name as placeholder, and the save settling on the
//! member's record).

use decibell_common::UserAvatar;
use decibell_core::api::community as api;
use decibell_core::events::CoreEvent;
use decibell_model::call::call;
use decibell_model::save_echo::EchoSave;
use decibell_model::{Model, ServerId, Uid};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, SectionLabel, Select, SelectOption, TextField,
    dialog_card,
};
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Subscription, WeakEntity, Window,
    div, prelude::*, px,
};

use super::UserMenu;
use crate::ui::{Look, button};

/// `NICKNAME_MAX_BYTES` (`features/servers/limits.ts`).
pub const NICKNAME_MAX_BYTES: usize = 32;
/// The reason's `maxLength`.
pub const REASON_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKind {
    Kick,
    Ban,
    Timeout,
}

/// `TIMEOUTS`.
pub const TIMEOUTS: [(i64, &str); 6] =
    [(60, "60 s"), (300, "5 min"), (600, "10 min"), (3600, "1 hour"), (86_400, "1 day"), (7 * 86_400, "1 week")];
const BAN_DURATIONS: [(i64, &str); 5] = [(0, "Permanent"), (3600, "1 hour"), (86_400, "1 day"), (7 * 86_400, "7 days"), (30 * 86_400, "30 days")];
const PURGES: [(i64, &str); 4] = [(0, "Keep all"), (3600, "Last hour"), (86_400, "Last 24 hours"), (7 * 86_400, "Last 7 days")];

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The member as the dialogs name them: (current name, nickname).
fn names(server: ServerId, uid: Uid, cx: &App) -> (SharedString, SharedString) {
    let member = Model::global(cx).community(server, cx).and_then(|c| c.roster.read(cx).member(uid).cloned());
    let fallback = member.as_ref().map(|m| m.username.clone()).unwrap_or_else(|| decibell_common::UNKNOWN_USER.into());
    let name = decibell_common::user_name(uid, &fallback, cx);
    (name, member.map(|m| m.nickname).unwrap_or_default())
}

/// "End timeout" (`runTimeout(m, true)`): no dialog.
pub fn end_timeout(server: ServerId, uid: Uid, cx: &mut App) {
    let (name, _) = names(server, uid, cx);
    let request = api::timeout_member(api::TimeoutMemberArgs { server_id: server.wire(), username: Some(name.to_string()), uid, until: 0, reason: Some(String::new()) });
    call(cx, request, |result, cx| {
        if let Err(e) = result {
            decibell_model::call::show_error(&e, "Action failed", cx);
        }
    });
}

pub struct ModerationDialog {
    kind: ModKind,
    server: ServerId,
    uid: Uid,
    menu: WeakEntity<UserMenu>,
    open: bool,
    reason: Entity<InputState>,
    ban_duration: i64,
    purge: i64,
    timeout: i64,
    busy: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ModerationDialog {
    pub fn new(kind: ModKind, server: ServerId, uid: Uid, menu: WeakEntity<UserMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let reason = cx.new(|cx| {
            InputState::multi_line(window, cx).placeholder("Optional. They see it, and so does the audit log.").max_length(REASON_MAX)
        });
        let subscriptions = vec![cx.subscribe(&reason, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Focus | InputEvent::Blur) {
                cx.notify();
            }
        })];
        Self {
            kind,
            server,
            uid,
            menu,
            open: true,
            reason,
            ban_duration: 0,
            purge: 0,
            timeout: 600,
            busy: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn kind(&self) -> ModKind {
        self.kind
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
        let _ = self.menu.update(cx, |menu, cx| menu.close_dialog(cx));
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (name, _) = names(self.server, self.uid, cx);
        let reason = self.reason.read(cx).value().trim().to_string();
        let (server_id, uid, username) = (self.server.wire(), self.uid, Some(name.to_string()));
        self.busy = true;
        self.error = None;
        cx.notify();
        let this = cx.weak_entity();
        let done = move |result: Result<(), decibell_model::UiError>, cx: &mut App| {
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.cancel(cx),
                    Err(e) => {
                        // `setError(String(err))`.
                        this.error = Some(e.js_string());
                        cx.notify();
                    }
                }
            });
        };
        match self.kind {
            ModKind::Kick => call(cx, api::kick_member(api::KickMemberArgs { server_id, username, uid, reason: Some(reason) }), done),
            ModKind::Ban => {
                let expires_at = if self.ban_duration > 0 { now_secs() + self.ban_duration } else { 0 };
                let args = api::BanMemberArgs {
                    server_id,
                    username,
                    uid,
                    reason: Some(reason),
                    expires_at: Some(expires_at),
                    delete_message_seconds: Some(self.purge as i32),
                };
                call(cx, api::ban_member(args), done)
            }
            ModKind::Timeout => {
                let args = api::TimeoutMemberArgs { server_id, username, uid, until: now_secs() + self.timeout, reason: Some(reason) };
                call(cx, api::timeout_member(args), done)
            }
        }
    }
}

impl Render for ModerationDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (name, nickname) = names(self.server, self.uid, cx);
        let shown: SharedString = if nickname.is_empty() { name.clone() } else { nickname.clone() };
        let (title, body, cta, icon) = match self.kind {
            ModKind::Kick => (
                format!("Kick {shown}"),
                "They're disconnected now and can come back with a valid invite.",
                "Kick",
                IconName::SettingsUserX,
            ),
            ModKind::Ban => (
                format!("Ban {shown}"),
                "They're disconnected and can't rejoin until the ban ends or someone lifts it.",
                "Ban",
                IconName::SettingsBan,
            ),
            ModKind::Timeout => (
                format!("Time out {shown}"),
                "They stay in the server but can't send messages, join voice or stream until it ends.",
                "Time out",
                IconName::SettingsClock,
            ),
        };
        let reason_field = TextField::new("mod-reason").editor(&self.reason).max_height(60.);
        let t = cx.theme();
        let (bg_dark, primary, muted, secondary, warning, error, on_error) =
            (t.color_bg_dark, t.color_text_primary, t.color_text_muted, t.color_text_secondary, t.color_warning, t.color_error, t.color_on_error);
        let (badge_bg, badge_fg) = if self.kind == ModKind::Timeout { (warning, bg_dark) } else { (error, on_error) };
        let options = |list: &[(i64, &'static str)]| list.iter().map(|(v, l)| SelectOption::new(*v, *l)).collect::<Vec<_>>();
        let extra = match self.kind {
            ModKind::Timeout => {
                let selected = self.timeout;
                let (accent, accent_soft, accent_bright, border, hover) =
                    (t.color_accent, t.color_accent_soft, t.color_accent_bright, t.color_border, t.color_surface_hover);
                let radius_sm = t.radius_sm;
                Some(
                    div()
                        .mb_4()
                        .child(div().mb_2().child(SectionLabel::new("Duration")))
                        .child(div().grid().grid_cols(6).gap_1p5().children(TIMEOUTS.iter().map(|(value, label)| {
                            let (value, on) = (*value, *value == selected);
                            div()
                                .id(("mod-timeout", value as u64))
                                .flex()
                                .justify_center()
                                .rounded(radius_sm)
                                .border_1()
                                .py_2()
                                .cursor_pointer()
                                .map(|this| {
                                    if on {
                                        this.border_color(accent).bg(accent_soft).text_color(accent_bright)
                                    } else {
                                        this.border_color(border).text_color(secondary).hover(move |s| s.bg(hover).text_color(primary))
                                    }
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.timeout = value;
                                    cx.notify();
                                }))
                                .child(Label::new(*label).size(12.).weight(600.))
                        })))
                        .into_any_element(),
                )
            }
            ModKind::Ban => Some(
                div()
                    .mb_4()
                    .grid()
                    .grid_cols(2)
                    .gap_3()
                    .child(
                        div().flex().flex_col().gap_2().child(SectionLabel::new("Duration")).child(
                            Select::options("mod-ban-duration", Some(self.ban_duration), options(&BAN_DURATIONS)).on_change(cx.listener(
                                |this, v: &i64, _, cx| {
                                    this.ban_duration = *v;
                                    cx.notify();
                                },
                            )),
                        ),
                    )
                    .child(
                        div().flex().flex_col().gap_2().child(SectionLabel::new("Delete their messages")).child(
                            Select::options("mod-ban-purge", Some(self.purge), options(&PURGES)).on_change(cx.listener(|this, v: &i64, _, cx| {
                                this.purge = *v;
                                cx.notify();
                            })),
                        ),
                    )
                    .into_any_element(),
            ),
            ModKind::Kick => None,
        };
        let busy = self.busy;
        let cancel = button("mod-cancel", "Cancel", Look::ghost(cx), false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)));
        let confirm = button("mod-confirm", if busy { "Working…" } else { cta }, Look::danger(cx), busy)
            .when(!busy, |this| this.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.confirm(cx))));
        let _ = window;
        let card = dialog_card(448., cx)
            .bg(bg_dark)
            .child(
                div()
                    .mb_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(UserAvatar::new(self.uid, name.clone(), 40.))
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(-4.))
                                    .right(px(-4.))
                                    .flex()
                                    .size_5()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .border_2()
                                    .border_color(bg_dark)
                                    .bg(badge_bg)
                                    .child(Icon::new(icon).size(px(11.)).stroke_width(2.75).color(badge_fg)),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .child(Label::new(SharedString::from(title)).font(FontRole::Display).size(16.).weight(600.).color(primary).truncate())
                            .when(!nickname.is_empty(), |this| this.child(Label::new(name.clone()).font(FontRole::Meta).size(cx.theme().scaled.text_meta).color(muted).truncate())),
                    ),
            )
            .child(div().mb_4().child(Label::new(body).size(13.).line_height(13. * 1.55).color(secondary)))
            .children(extra)
            .child(div().mb_5().flex().flex_col().gap_2().child(SectionLabel::new("Reason")).child(reason_field))
            .when_some(self.error.clone(), |this, e| this.child(div().mb_3().child(Label::new(e).size(13.).color(error))))
            .child(div().flex().justify_end().gap_2().child(cancel).child(confirm));
        Modal::new(("mod-dialog", self.uid as u64), self.open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.cancel(cx)))
            .child(card)
    }
}

pub struct NicknameDialog {
    server: ServerId,
    uid: Uid,
    menu: WeakEntity<UserMenu>,
    open: bool,
    field: Entity<InputState>,
    save: EchoSave<String>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl NicknameDialog {
    pub fn new(server: ServerId, uid: Uid, menu: WeakEntity<UserMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let (name, nickname) = names(server, uid, cx);
        let field = cx.new(|cx| InputState::single_line(window, cx).placeholder(name.clone()).max_bytes(NICKNAME_MAX_BYTES).with_value(&nickname));
        field.update(cx, |state, cx| state.focus(window, cx));
        let mut subscriptions = vec![
            cx.subscribe(&field, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.submit(cx),
                InputEvent::Escape => this.cancel(cx),
                _ => {}
            }),
            // A refusal (the model toasts it) leaves the dialog open.
            cx.subscribe(&model.answers, move |this, _, event: &CoreEvent, cx| {
                if let CoreEvent::ModActionResponded(p) = event
                    && p.action == "nickname"
                    && !p.success
                    && ServerId::from_wire(&p.server_id) == this.server
                {
                    this.save.stop();
                    cx.notify();
                }
            }),
        ];
        if let Some(community) = model.community(server, cx) {
            subscriptions.push(cx.observe(&community.roster, move |this, roster, cx| {
                let shown = roster.read(cx).member(uid).map(|m| m.nickname.to_string());
                if this.save.settle(|sent| shown.as_deref() == Some(sent.as_str())).is_some() {
                    this.cancel(cx);
                }
            }));
        }
        Self { server, uid, menu, open: true, field, save: EchoSave::default(), error: None, _subscriptions: subscriptions }
    }

    pub fn field(&self) -> &Entity<InputState> {
        &self.field
    }

    pub fn saving(&self) -> bool {
        self.save.saving()
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
        let _ = self.menu.update(cx, |menu, cx| menu.close_dialog(cx));
    }

    /// `runSetNickname`: the trimmed text (empty clears it), held until the record shows it.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.save.saving() {
            return;
        }
        let nickname = self.field.read(cx).value().trim().to_string();
        let (name, _) = names(self.server, self.uid, cx);
        self.error = None;
        self.save.start(nickname.clone(), cx, |this: &mut NicknameDialog| &mut this.save);
        let this = cx.weak_entity();
        let args = api::SetNicknameArgs { server_id: self.server.wire(), username: Some(name.to_string()), uid: self.uid, nickname };
        call(cx, api::set_nickname(args), move |result, cx| {
            if let Err(e) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(e.js_string());
                    this.save.stop();
                    cx.notify();
                });
            }
        });
    }
}

impl Render for NicknameDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, nickname) = names(self.server, self.uid, cx);
        let title = if nickname.is_empty() { "Set nickname" } else { "Change nickname" };
        let saving = self.save.saving();
        let cancel = button("nick-cancel", "Cancel", Look::ghost(cx), false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)));
        let save = button("nick-save", if saving { "Saving…" } else { "Save" }, Look::primary(cx), saving)
            .when(!saving, |this| this.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit(cx))));
        let t = cx.theme();
        let (primary, error) = (t.color_text_primary, t.color_error);
        let card = dialog_card(400., cx)
            .child(div().mb_4().child(Label::new(title).font(FontRole::Display).size(16.).weight(600.).color(primary)))
            .child(div().mb_5().child(TextField::new("nickname-field").editor(&self.field)))
            .when_some(self.error.clone(), |this, e| this.child(div().mb_3().child(Label::new(e).size(13.).color(error))))
            .child(div().flex().justify_end().gap_2().child(cancel).child(save));
        Modal::new(("nickname-dialog", self.uid as u64), self.open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.cancel(cx)))
            .child(card)
    }
}
