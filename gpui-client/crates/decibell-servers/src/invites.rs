//! The Invites dialog (`features/servers/InviteModal.tsx`, `modal::INVITE_MANAGE`): create an
//! invite (expiry, max uses), the list (the community answers with yours, or everyone's with
//! MANAGE_INVITES), copy a code or its code-only link (`decibell://invite/<CODE>`), revoke.
//!
//! The list is windowed: each row's height is known before it is drawn (its meta line wraps as
//! `flex-wrap` would, measured once per list and width).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use decibell_common::Watch;
use decibell_core::api;
use decibell_model::error::UiError;
use decibell_model::invite_link::build_invite_link;
use decibell_model::regional::Regional;
use decibell_model::stores::community::Invite;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, ServerId};
use decibell_text::input::{InputEvent, InputState};
use decibell_text::{Engine, Wrapping};
use decibell_ui::{
    ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, RowHeights, ScrollArea, ScrollHandle, Select,
    SelectOption, TextField, visible_window_bounds,
};
use gpui::{Focusable as _, 
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, IntoElement, Keystroke, Render, ScrollEvent,
    SharedString, Task, Window, canvas, div, prelude::*, px,
};

use crate::follow::Follow;
use crate::ui::{self, HEADING_TRACKING};

/// `EXPIRY_OPTIONS`.
pub const EXPIRY_OPTIONS: [(&str, i64); 5] =
    [("1 hour", 3600), ("1 day", 86400), ("7 days", 604800), ("30 days", 2592000), ("Never", 0)];

/// The list's padding and the card's width less it: `max-w-[440px]`, `px-6`, a row's `p-3.5`
/// and border.
const CARD_WIDTH: f32 = 440.;

/// What the dialog shows (for the screens' fixture too).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Gate {
    open: bool,
    server: Option<ServerId>,
    can_manage: bool,
}

pub struct InviteManageOverlay {
    model: Model,
    gate: Gate,
    expiry: i64,
    unlimited: bool,
    max_uses: Entity<InputState>,
    creating: bool,
    error: Option<SharedString>,
    copied: Option<SharedString>,
    scroll: ScrollHandle,
    /// The rows' text width as last laid out (the scrollbar takes 10 px when the list overflows).
    measured: Rc<Cell<f32>>,
    follow: Follow,
    _copied: Option<Task<()>>,
    _list: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
    _watch: Watch,
}

impl InviteManageOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let max_uses = cx.new(|cx| {
            let mut state = InputState::single_line(window, cx).placeholder("10").with_value("10");
            // ↑ / ↓ step the number (handled on the box).
            state.pass_keys(|k: &Keystroke| !k.modifiers.modified() && (k.key == "up" || k.key == "down"));
            state
        });
        cx.subscribe(&max_uses, |this, state, event: &InputEvent, cx| {
            if *event == InputEvent::Escape {
                // `useEscapeToClose`: Escape in the field closes too.
                this.close(cx);
                return;
            }
            if *event != InputEvent::Change {
                return;
            }
            // Digits only, at most 1000 (`onChange`); empty is allowed.
            let value = state.read(cx).value().to_string();
            let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
            let next = if digits.is_empty() { String::new() } else { digits.parse::<u64>().unwrap_or(u64::MAX).min(1000).to_string() };
            if next != value {
                state.update(cx, |s, cx| s.set_value(&next, cx));
            }
            cx.notify();
        })
        .detach();
        let watch = Watch::new();
        let mut subscriptions: Vec<gpui::Subscription> = Vec::new();
        subscriptions.push(cx.observe(&model.overlays, |this: &mut Self, _, cx| {
            if this.sync(cx) {
                cx.notify();
            }
        }));
        subscriptions.push(cx.observe(&model.nav, |this: &mut Self, _, cx| {
            if this.sync(cx) {
                cx.notify();
            }
        }));
        let mut this = Self {
            model,
            gate: Gate { open: false, server: None, can_manage: false },
            expiry: 604800,
            unlimited: true,
            max_uses,
            creating: false,
            error: None,
            copied: None,
            scroll: ScrollHandle::new(),
            measured: Rc::new(Cell::new(0.)),
            follow: Follow::default(),
            _copied: None,
            _list: None,
            _subscriptions: subscriptions,
            _watch: watch,
        };
        this.sync(cx);
        cx.set_global(crate::Current::<InviteManageOverlay>(cx.weak_entity()));
        this
    }

    fn gate(&self, cx: &App) -> Gate {
        let model = &self.model;
        let open = model.overlays.read(cx).is_open(modal::INVITE_MANAGE);
        let server = model.nav.read(cx).active_server;
        // Permissions v3: CREATE_INVITE creates and sees / revokes your own invites;
        // MANAGE_INVITES sees and revokes everyone's (and implies create).
        let can_manage = server.is_some_and(|s| model.has_permission(s, perm::CREATE_INVITE, cx) || model.has_permission(s, perm::MANAGE_INVITES, cx));
        Gate { open, server, can_manage }
    }

    /// The list request (`useEffect([activeModal, activeServerId, canManage])`); whether
    /// anything shown changed.
    fn sync(&mut self, cx: &mut Context<Self>) -> bool {
        let gate = self.gate(cx);
        if gate == self.gate {
            return false;
        }
        let was_open = self.gate.open;
        self.gate = gate;
        if gate.open && !was_open {
            // The component's state lives as long as MainLayout: only the list scroll restarts.
            self.scroll = ScrollHandle::new();
            if crate::screen_state(cx).is_some_and(|s| s.invite_creating) {
                self.creating = true;
            }
        }
        self.follow.set(gate.open.then_some(gate.server).flatten(), cx);
        if gate.open
            && gate.can_manage
            && decibell_model::core::is_running()
            && let Some(server) = gate.server
        {
            let args = api::community::ListInvitesArgs { server_id: server.wire() };
            self._list = Some(cx.spawn(async move |this, cx| {
                let result = if decibell_model::core::is_running() {
                    api::community::list_invites(args).await.map_err(UiError::from)
                } else {
                    Err(UiError::not_running())
                };
                if let Err(e) = result {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(e.js_string());
                        cx.notify();
                    });
                }
            }));
        }
        true
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.model.overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    fn parsed_max_uses(&self, cx: &App) -> Option<i64> {
        ui::parse_int(self.max_uses.read(cx).value())
    }

    fn max_uses_valid(&self, cx: &App) -> bool {
        self.unlimited || self.parsed_max_uses(cx).is_some_and(|n| (1..=1000).contains(&n))
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let current = self.parsed_max_uses(cx).unwrap_or(0);
        let next = (current + delta).clamp(1, 1000).to_string();
        self.max_uses.update(cx, |s, cx| s.set_value(&next, cx));
    }

    /// `handleCreate`.
    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.gate.server else { return };
        self.creating = true;
        self.error = None;
        cx.notify();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let expires_at = if self.expiry == 0 { 0 } else { now + self.expiry };
        let max_uses = if self.unlimited { 0 } else { self.parsed_max_uses(cx).unwrap_or(0) as i32 };
        let args = api::community::CreateInviteArgs {
            server_id: server.wire(),
            expires_at,
            max_uses,
            for_user: None,
            for_uid: None,
            request_id: None,
        };
        cx.spawn(async move |this, cx| {
            let result = if decibell_model::core::is_running() {
                api::community::create_invite(args).await.map_err(UiError::from)
            } else {
                Err(UiError::not_running())
            };
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(e.js_string());
                }
                this.creating = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn revoke(&mut self, code: SharedString, cx: &mut Context<Self>) {
        let Some(server) = self.gate.server else { return };
        let args = api::community::RevokeInviteArgs { server_id: server.wire(), code: code.to_string() };
        cx.spawn(async move |this, cx| {
            let result = if decibell_model::core::is_running() {
                api::community::revoke_invite(args).await.map_err(UiError::from)
            } else {
                Err(UiError::not_running())
            };
            if let Err(e) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(e.js_string());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `handleCopy` / `handleCopyCode`: the clipboard, then "Copied!" for 1.5 s.
    fn copy(&mut self, text: String, key: SharedString, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key.clone());
        cx.notify();
        self._copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.as_ref() == Some(&key) {
                    this.copied = None;
                    cx.notify();
                }
            });
        }));
    }

    /// For the self-tests: the form as the user would fill it (the Select's choice, Unlimited off
    /// and a number, or on).
    pub fn set_form(&mut self, expiry: i64, max_uses: Option<u32>, cx: &mut Context<Self>) {
        self.expiry = expiry;
        self.unlimited = max_uses.is_none();
        if let Some(n) = max_uses {
            self.max_uses.update(cx, |s, cx| s.set_value(&n.to_string(), cx));
        }
        cx.notify();
    }

    /// For the screens: the form as it stands while a create is out.
    pub fn set_creating(&mut self, creating: bool, cx: &mut Context<Self>) {
        self.creating = creating;
        cx.notify();
    }
}

impl Render for InviteManageOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let gate = self.gate;
        let open = gate.open && gate.server.is_some();
        let card = open.then(|| if gate.can_manage { self.card(window, cx).into_any_element() } else { self.no_permission(cx).into_any_element() });
        Modal::new("invite-manage", open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.close(cx)))
            .children(card)
    }
}

/// One invite row, ready to draw.
struct Row {
    invite: Invite,
    creator: SharedString,
    for_name: Option<SharedString>,
    uses: SharedString,
    expires: SharedString,
    height: f32,
}

impl InviteManageOverlay {
    fn no_permission(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.theme();
        let (bg_light, bg_lighter) = (t.color_bg_light, t.color_bg_lighter);
        div()
            .occlude()
            .w_full()
            .max_w(px(384.))
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_dark)
            .p_6()
            .shadow(t.shadow_modal.clone())
            .child(div().mb_2().child(Label::new("Invites").font(FontRole::Display).size(18.).line_height(28.).weight(600.).tracking(HEADING_TRACKING).color(t.color_text_primary)))
            .child(Label::new("You don't have permission to create invites.").size(13.).line_height(19.5).color(t.color_text_secondary))
            .child(
                div()
                    .id("invites-close-plain")
                    .mt_4()
                    .w_full()
                    .flex()
                    .justify_center()
                    .rounded(t.radius_md)
                    .bg(bg_light)
                    .px_4()
                    .py_2p5()
                    .cursor_pointer()
                    .hover(move |s| s.bg(bg_lighter))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                    .child(Label::new("Close").size(13.).line_height(19.5).weight(500.).color(t.color_text_primary)),
            )
    }

    fn rows(&self, invites: &[Invite], text_width: f32, scale: f32, cx: &mut App) -> Vec<Row> {
        let format = self.model.settings.read(cx).time_format();
        let regional = Regional::get();
        let style = ui::text_style(FontRole::Sans, 11., 16.5, cx);
        let engine = Engine::global(cx);
        let width_of = |text: &str| engine.label_layout(text, &style, Wrapping::NOWRAP, f32::INFINITY, scale).width();
        invites
            .iter()
            .map(|invite| {
                let creator = decibell_common::user_name(invite.created_by_uid, &invite.created_by, cx);
                let for_name =
                    (!invite.for_user.is_empty()).then(|| decibell_common::user_name(invite.for_uid, &invite.for_user, cx));
                let uses: SharedString = if invite.max_uses == 0 {
                    format!("{} / ∞", regional.group(invite.uses.max(0) as u64)).into()
                } else {
                    format!("{} / {}", regional.group(invite.uses.max(0) as u64), regional.group(invite.max_uses.max(0) as u64)).into()
                };
                let expires: SharedString =
                    if invite.expires_at == 0 { "Never".into() } else { regional.date_time(invite.expires_at, format).into() };
                // `flex flex-wrap gap-x-4 gap-y-1`: the three items packed onto lines.
                let mut items = vec![12. + 6. + width_of(&creator) + for_name.as_ref().map_or(0., |n| 6. + width_of(&format!("→ {n}")))];
                items.push(12. + 6. + width_of(&uses));
                items.push(12. + 6. + width_of(&expires));
                let mut lines = 1;
                let mut used = 0.;
                for (i, w) in items.iter().enumerate() {
                    if i > 0 && used + 16. + w > text_width + 0.01 {
                        lines += 1;
                        used = *w;
                    } else {
                        used += if i > 0 { 16. } else { 0. } + w;
                    }
                }
                // Border, `p-3.5`, the code button (31.5), `mt-2.5`, the meta lines.
                let height = 2. + 28. + 31.5 + 10. + 16.5 * lines as f32 + 4. * (lines - 1) as f32;
                Row { invite: invite.clone(), creator, for_name, uses, expires, height }
            })
            .collect()
    }

    fn card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let server = self.gate.server.expect("open with a server");
        let model = self.model.clone();
        let can_see_all = model.has_permission(server, perm::MANAGE_INVITES, cx);
        let (invites, truncated) = model
            .community(server, cx)
            .map(|c| {
                let m = c.moderation.read(cx);
                (m.invites.clone(), m.invites_truncated)
            })
            .unwrap_or_default();
        let scale = window.scale_factor();
        let measured = match self.measured.get() {
            w if w > 0. => w,
            _ => CARD_WIDTH - 48. - 28. - 2.,
        };
        let rows = self.rows(&invites, measured, scale, cx);
        let valid = self.max_uses_valid(cx);
        let t = cx.theme();
        let window_height = f32::from(visible_window_bounds(window, cx).size.height);

        // Header.
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(t.color_border_divider)
            .px_6()
            .py_5()
            .child(Label::new("Invites").font(FontRole::Display).size(18.).line_height(27.).weight(600.).tracking(HEADING_TRACKING).color(t.color_text_primary))
            .child(ui::close_button("invites-close", cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)), cx));

        // The create form.
        let section_label = |text: &'static str, cx: &App| {
            Label::new(text.to_uppercase()).size(11.).line_height(16.5).weight(600.).tracking(0.07).color(cx.theme().color_text_muted)
        };
        let field_label = |text: &'static str, cx: &App| {
            div().mb_1p5().child(Label::new(text).size(12.).line_height(18.).weight(500.).color(cx.theme().color_text_secondary))
        };
        let expiry = Select::options("invite-expiry", Some(self.expiry), EXPIRY_OPTIONS.iter().map(|(label, s)| SelectOption::new(*s, *label)).collect())
            .on_change(cx.listener(|this, value: &i64, _, cx| {
                this.expiry = *value;
                cx.notify();
            }));
        let max_uses = self.max_uses_box(window, cx);
        let t = cx.theme();
        let (accent, accent_hover, on_accent) = (t.color_accent, t.color_accent_hover, t.color_on_accent);
        let can_create = !self.creating && valid;
        let create = div()
            .id("invite-create")
            .relative()
            .children(decibell_shell::probes::probe("invite-create"))
            .w_full()
            .flex()
            .justify_center()
            .rounded(t.radius_md)
            .bg(accent)
            .py(px(11.))
            .when(can_create, |this| this.cursor_pointer().hover(move |s| s.bg(accent_hover)))
            .when(!can_create, |this| this.opacity(0.5))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if can_create {
                    this.create(cx)
                }
            }))
            .child(Label::new(if self.creating { "Creating..." } else { "Create Invite" }).size(13.).line_height(19.5).weight(600.).color(on_accent));
        let form = div()
            .flex_none()
            .px_6()
            .py_5()
            .child(div().mb(px(14.)).child(section_label("Create invite", cx)))
            .child(
                div()
                    .mb(px(14.))
                    .flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(field_label("Expires", cx)).child(expiry))
                    .child(div().flex_1().min_w_0().child(field_label("Max uses", cx)).child(max_uses)),
            )
            .child(create)
            .children(self.error.clone().map(|e| div().mt_2p5().child(Label::new(e).size(12.).line_height(18.).color(cx.theme().color_error))));
        let form_height = 40. + 16.5 + 14. + 18. + 6. + 40. + 14. + 41.5 + if self.error.is_some() { 10. + 18. } else { 0. };

        // The list: its label (and the truncation note) is its first row.
        let t = cx.theme();
        let label: SharedString = format!(
            "{} ({})",
            if can_see_all { "Active invites" } else { "Your invites" },
            Regional::get().group(invites.len() as u64)
        )
        .to_uppercase()
        .into();
        let head_height = 20. + 16.5 + 12. + if truncated { 18. + 12. } else { 0. };
        let available = (window_height * 0.8 - 69. - form_height - 1.).max(0.);
        let list = if rows.is_empty() {
            let height: f32 = head_height + 20. + 40. + 10. + 19.5 + 20. + 20.;
            div()
                .flex_none()
                .h(px(height.min(available)))
                .overflow_hidden()
                .px_6()
                .pt_5()
                .child(div().mb_3().child(Label::new(label).size(11.).line_height(16.5).weight(600.).tracking(0.07).color(t.color_text_muted)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2p5()
                        .py_5()
                        .child(
                            div()
                                .flex()
                                .size_10()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(t.color_text_muted.opacity(0.15))
                                .child(Icon::new(IconName::SettingsLink).size(px(18.)).color(t.color_text_muted)),
                        )
                        .child(Label::new("No invites yet. Create one above.").size(13.).line_height(19.5).color(t.color_text_muted)),
                )
                .into_any_element()
        } else {
            let count = rows.len() + 1;
            let mut offsets = Vec::with_capacity(count + 1);
            offsets.push(px(0.));
            let mut top = head_height;
            offsets.push(px(top));
            for (i, row) in rows.iter().enumerate() {
                top += row.height + if i + 1 == rows.len() { 20. } else { 8. };
                offsets.push(px(top));
            }
            let total = top;
            let rows = Rc::new(rows);
            let view = cx.entity().downgrade();
            let measured = self.measured.clone();
            let copied = self.copied.clone();
            let note = truncated.then(|| format!("Showing the most recent {} invites.", Regional::get().group(invites.len() as u64)));
            ScrollArea::new("invite-list")
                .track_scroll(&self.scroll)
                .flex_none()
                .h(px(total.min(available)))
                .windowed(count, RowHeights::Offsets(offsets.into()), move |range, _, cx| {
                    range
                        .map(|i| {
                            if i == 0 {
                                let t = cx.theme();
                                return div()
                                    .px_6()
                                    .pt_5()
                                    .child(div().mb_3().child(Label::new(label.clone()).size(11.).line_height(16.5).weight(600.).tracking(0.07).color(t.color_text_muted)))
                                    .children(note.clone().map(|n| div().mb_3().child(Label::new(n).size(12.).line_height(18.).color(t.color_text_muted))))
                                    .into_any_element();
                            }
                            invite_row(&rows[i - 1], copied.as_ref(), view.clone(), measured.clone(), cx)
                        })
                        .collect()
                })
                .into_any_element()
        };

        div()
            .id("invite-card")
            .occlude()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(CARD_WIDTH))
            .overflow_hidden()
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_dark)
            .shadow(t.shadow_modal.clone())
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                // Escape (`useEscapeToClose`) is the Modal's; ↑ / ↓ step the max uses.
                let key = e.keystroke.key.as_str();
                if !this.unlimited && this.max_uses.read(cx).focus_handle(cx).is_focused(window) {
                    match key {
                        "up" => this.step(1, cx),
                        "down" => this.step(-1, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(form)
            .child(div().h(px(1.)).flex_none().bg(t.color_border_divider))
            .child(list)
    }

    /// Max uses: the Unlimited box, then ∞ or the number with its steppers.
    fn max_uses_box(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let focus = self.max_uses.read(cx).focus_handle(cx);
        let focused = focus.contains_focused(window, cx);
        let t = cx.theme();
        let (surface_hover, accent, border, divider, muted, secondary) =
            (t.color_surface_hover, t.color_accent, t.color_border, t.color_border_divider, t.color_text_muted, t.color_text_secondary);
        let unlimited = self.unlimited;
        let check = div()
            .id("invite-unlimited")
            .flex()
            .h_full()
            .flex_none()
            .items_center()
            .gap(px(7.))
            .border_r_1()
            .border_color(divider)
            .px_3()
            .cursor_pointer()
            .hover(move |s| s.bg(surface_hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.unlimited = !this.unlimited;
                if !this.unlimited {
                    // `autoFocus` on the number as it appears.
                    this.max_uses.update(cx, |s, cx| s.focus(window, cx));
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .size_4()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(t.radius_sm)
                    .map(|this| if unlimited { this.border_1().border_color(accent).bg(accent) } else { this.border(px(1.5)).border_color(border) })
                    .when(unlimited, |this| this.child(Icon::new(IconName::ServersCheck).size(px(10.)).color(gpui::white()))),
            )
            .child(Label::new("Unlimited").size(12.).line_height(18.).weight(500.).color(secondary).nowrap());
        let input_style = TextField::input_style(FontRole::Sans, 13., 1.5, cx);
        let t = cx.theme();
        div()
            .flex()
            .h(px(40.))
            .items_center()
            .overflow_hidden()
            .rounded(t.radius_md)
            .border_1()
            .border_color(if focused { accent } else { border })
            .bg(t.color_bg_lighter)
            .when(focused, |this| this.shadow(t.shadow_ring.clone()))
            .child(check)
            .map(|this| {
                if unlimited {
                    this.child(div().flex().flex_1().items_center().justify_center().child(Label::new("∞").size(18.).line_height(27.).color(muted)))
                } else {
                    let step = |id: &'static str, icon: IconName, delta: i64, top: bool, cx: &mut Context<Self>| {
                        div()
                            .id(id)
                            .group(id)
                            .flex()
                            .flex_1()
                            .w_7()
                            .items_center()
                            .justify_center()
                            .when(top, |this| this.border_b_1().border_color(divider))
                            .cursor_pointer()
                            .hover(move |s| s.bg(surface_hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.step(delta, cx)))
                            .child(Icon::new(icon).size(px(10.)).color(muted).group_hover(id, secondary))
                    };
                    this.child(
                        div()
                            .id("invite-max-uses")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .px_3()
                            .track_focus(&focus)
                            .cursor_text()
                            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                                let dy = f32::from(e.delta.pixel_delta(px(16.)).y);
                                if dy > 0. {
                                    this.step(1, cx)
                                } else if dy < 0. {
                                    this.step(-1, cx)
                                }
                            }))
                            .child(div().flex_1().min_w_0().child(decibell_text::input::TextInput::new(&self.max_uses, input_style))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .h_full()
                            .flex_none()
                            .border_l_1()
                            .border_color(divider)
                            .child(step("invite-step-up", IconName::ServersStepUp, 1, true, cx))
                            .child(step("invite-step-down", IconName::ServersStepDown, -1, false, cx)),
                    )
                }
            })
    }
}

/// One invite's card.
fn invite_row(
    row: &Row,
    copied: Option<&SharedString>,
    view: gpui::WeakEntity<InviteManageOverlay>,
    measured: Rc<Cell<f32>>,
    cx: &mut App,
) -> AnyElement {
    let t = cx.theme();
    let code = row.invite.code.clone();
    let raw_copied = copied.is_some_and(|c| c.as_ref() == format!("raw:{code}"));
    let link_copied = copied.is_some_and(|c| *c == code);
    let (bg_mid, bg_lighter, accent_soft, accent_mid, error) = (t.color_bg_mid, t.color_bg_lighter, t.color_accent_soft, t.color_accent_mid, t.color_error);
    let (muted, secondary) = (t.color_text_muted, t.color_text_secondary);
    let group: SharedString = format!("invite-code-{code}").into();
    let copy = {
        let view = view.clone();
        move |text: String, key: SharedString| {
            let view = view.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = view.update(cx, |this, cx| this.copy(text.clone(), key.clone(), cx));
            }
        }
    };
    let meta_item = |icon: IconName, children: Vec<AnyElement>| {
        div().flex().items_center().gap_1p5().child(Icon::new(icon).size(px(12.)).color(muted)).children(children)
    };
    let meta_text = |text: SharedString| Label::new(text).size(11.).line_height(16.5).color(muted).nowrap().into_any_element();
    let mut creator = vec![meta_text(row.creator.clone())];
    if let Some(name) = &row.for_name {
        creator.push(
            div()
                .id("invite-for")
                .tooltip(decibell_ui::title_tooltip("A personal invite: only they can use it"))
                .child(meta_text(format!("→ {name}").into()))
                .into_any_element(),
        );
    }
    div()
        .id(gpui::ElementId::Name(format!("invite-{code}").into()))
        .relative()
        .children(decibell_shell::probes::probe(format!("invite:{code}")))
        .mx_6()
        .h(px(row.height))
        .rounded(t.radius_md)
        .border_1()
        .border_color(t.color_border_divider)
        .bg(t.color_bg_light)
        .p(px(14.))
        .child(
            // The text width, for the next layout.
            canvas(
                {
                    let view = view.clone();
                    move |bounds, _, cx| {
                    let width = f32::from(bounds.size.width);
                    if (width - measured.get()).abs() > 0.5 {
                        measured.set(width);
                        if let Some(view) = view.upgrade() {
                            cx.notify(view.entity_id());
                        }
                    }
                }},
                |_, _, _, _| {},
            )
            .absolute()
            .left(px(14.))
            .right(px(14.))
            .h_0(),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(
                    div()
                        .id("invite-code")
                        .relative()
                        .children(decibell_shell::probes::probe(format!("invite-copy-code:{code}")))
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(t.radius_sm)
                        .bg(bg_mid)
                        .px_2p5()
                        .py_1p5()
                        .cursor_pointer()
                        .hover(move |s| s.bg(bg_lighter))
                        .tooltip(decibell_ui::title_tooltip("Copy code"))
                        .on_click(copy(code.to_string(), format!("raw:{code}").into()))
                        .child(Label::new(code.clone()).font(FontRole::Mono).size(13.).line_height(19.5).weight(600.).tracking(0.07).color(t.color_accent_bright))
                        .child(
                            div()
                                .text_color(muted)
                                .group_hover(group, move |s| s.text_color(secondary))
                                // Inside the `font-mono` button: monospace too.
                                .child(Label::new(if raw_copied { "Copied!" } else { "Copy" }).font(FontRole::Mono).size(10.).line_height(15.)),
                        ),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .gap_1p5()
                        .child(
                            div()
                                .id("invite-copy-link")
                                .relative()
                                .children(decibell_shell::probes::probe(format!("invite-copy:{code}")))
                                .rounded(t.radius_sm)
                                .bg(accent_soft)
                                .px_3()
                                .py_1p5()
                                .cursor_pointer()
                                .hover(move |s| s.bg(accent_mid))
                                .on_click(copy(build_invite_link(&code), code.clone()))
                                .child(Label::new(if link_copied { "Copied!" } else { "Copy link" }).size(11.).line_height(16.5).weight(500.).color(t.color_accent_bright)),
                        )
                        .child(
                            div()
                                .id("invite-revoke")
                                .relative()
                                .children(decibell_shell::probes::probe(format!("invite-revoke:{code}")))
                                .rounded(t.radius_sm)
                                .bg(error.opacity(0.1))
                                .px_3()
                                .py_1p5()
                                .cursor_pointer()
                                .hover(move |s| s.bg(error.opacity(0.2)))
                                .on_click({
                                    let (view, code) = (view.clone(), code.clone());
                                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                                        let _ = view.update(cx, |this, cx| this.revoke(code.clone(), cx));
                                    }
                                })
                                .child(Label::new("Revoke").size(11.).line_height(16.5).weight(500.).color(error)),
                        ),
                ),
        )
        .child(
            div()
                .mt_2p5()
                .flex()
                .flex_wrap()
                .gap_x_4()
                .gap_y_1()
                .child(meta_item(IconName::ServersUser, creator))
                .child(meta_item(IconName::SettingsUsers, vec![meta_text(row.uses.clone())]))
                .child(meta_item(IconName::ServersClock, vec![meta_text(row.expires.clone())])),
        )
        .into_any_element()
}
