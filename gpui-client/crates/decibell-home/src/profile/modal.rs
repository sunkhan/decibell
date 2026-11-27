//! The profile screen (`UserProfileModal.tsx`): a 640 × 560 card over the window, reached from
//! the popout's avatar. The banner, the avatar cut into it, the friendship / message / edit
//! actions, the identity, then two tabs. **Profile**: About (username, nickname, status,
//! friendship, joined) and the roles in the community it was opened from. **Privacy**: end-to-end
//! encryption — our own fingerprint, or for someone else the conversation's safety number, their
//! fingerprint, the pin and the last key change.

use decibell_common::{StatusMessageText, UserAvatar, Watch, dot_presence, presence_of, status_message_of, want_member};
use decibell_core::api;
use decibell_model::live::e2ee::E2eeStatus;
use decibell_model::live::friends::{Friend, FriendAction, FriendStatus};
use decibell_model::stores::community::{Member, Role};
use decibell_model::stores::overlays::modal;
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_text::input::{InputState, InputStyle, TextArea, TextInput};
use decibell_ui::{ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, ScrollArea, StatusDot};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, Global, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};

use super::role_chips;
use crate::e2ee::{PeerInfo, PeerInfoChanged, PeerInfos, current_key};
use crate::ui::{Look, button, lock_glyph, person_gradient, status_pill};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileTab {
    #[default]
    Profile,
    Privacy,
}

/// The tab the next opening starts on (a fresh opening otherwise starts on Profile, as the
/// renderer keys the screen by user).
#[derive(Default)]
struct TabRequest(Option<ProfileTab>);

impl Global for TabRequest {}

/// Open the profile screen for `uid` (from `server`, for its nickname and roles) on `tab`.
pub fn open_profile(uid: Uid, server: Option<ServerId>, tab: ProfileTab, cx: &mut App) {
    cx.default_global::<TabRequest>().0 = Some(tab);
    let overlays = Model::global(cx).overlays.clone();
    overlays.update(cx, |o, cx| o.open_user_profile(uid, server, cx));
}

pub struct ProfileModal {
    /// The person shown and the community it was opened from (kept through the fade-out).
    shown: Option<(Uid, Option<ServerId>)>,
    open: bool,
    tab: ProfileTab,
    /// A friend action on its way (`FriendActions`' `busy`).
    busy: bool,
    /// The `select-text` values drawn (About's values, the safety number's groups, fingerprints):
    /// read-only fields, by place, made when first drawn and dropped when the screen closes.
    values: std::collections::HashMap<SharedString, Entity<InputState>>,
    community: Vec<Subscription>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl ProfileModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .people(cx, |this: &ProfileModal, uid| this.open && this.shown.is_some_and(|(u, _)| u == uid))
            .store_if(&model.live.friends, cx, |this: &mut ProfileModal, _, _| this.open)
            .store_if(&model.live.e2ee, cx, |this: &mut ProfileModal, _, _| this.open);
        let peer_infos = PeerInfos::entity(cx);
        let subscriptions = vec![
            cx.observe(&model.overlays, |this, overlays, cx| {
                let o = overlays.read(cx);
                let open = o.is_open(modal::USER_PROFILE) && o.user_profile_user.is_some();
                let next = o.user_profile_user.map(|uid| (uid, o.user_profile_server));
                this.overlays_changed(open, next, cx)
            }),
            cx.subscribe(&peer_infos, |this, _, event: &PeerInfoChanged, cx| {
                if this.open && this.shown.is_some_and(|(u, _)| u == event.0) {
                    cx.notify();
                }
            }),
            // Our status reaching "ready" asks for the safety number.
            cx.observe(&model.live.e2ee, |this, _, cx| this.want_info(cx)),
        ];
        let mut this = Self {
            shown: None,
            open: false,
            tab: ProfileTab::Profile,
            busy: false,
            values: Default::default(),
            community: Vec::new(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        let (open, next) = {
            let o = model.overlays.read(cx);
            (o.is_open(modal::USER_PROFILE) && o.user_profile_user.is_some(), o.user_profile_user.map(|uid| (uid, o.user_profile_server)))
        };
        this.overlays_changed(open, next, cx);
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        this
    }

    fn overlays_changed(&mut self, open: bool, next: Option<(Uid, Option<ServerId>)>, cx: &mut Context<Self>) {
        if open && (!self.open || next != self.shown) {
            // A new opening: keyed by the person, starting on the requested tab or Profile.
            self.tab = cx.default_global::<TabRequest>().0.take().unwrap_or_default();
            self.shown = next;
            self.busy = false;
            self.values.clear();
            self.follow_community(cx);
            self.open = true;
            self.want_info(cx);
            cx.notify();
        } else if !open && self.open {
            self.open = false;
            self.community.clear();
            self.values.clear();
            cx.notify();
        }
    }

    fn follow_community(&mut self, cx: &mut Context<Self>) {
        self.community.clear();
        let Some((uid, Some(server))) = self.shown else { return };
        let Some(community) = Model::global(cx).community(server, cx) else { return };
        if community.roster.read(cx).member(uid).is_none() {
            want_member(server, uid, cx);
        }
        self.community.push(cx.observe(&community.roster, |_, _, cx| cx.notify()));
        self.community.push(cx.observe(&community.roles, |_, _, cx| cx.notify()));
        self.community.push(cx.observe(&community.meta, |_, _, cx| cx.notify()));
    }

    /// The safety number needs both identities: asked for once ours is unlocked.
    fn want_info(&mut self, cx: &mut Context<Self>) {
        let Some((uid, _)) = self.shown.filter(|_| self.open) else { return };
        let model = Model::global(cx).clone();
        if uid != model.auth.read(cx).uid && model.live.e2ee.read(cx).status == E2eeStatus::Ready {
            PeerInfos::want(uid, cx);
        }
    }

    pub fn tab(&self) -> ProfileTab {
        self.tab
    }

    /// A selectable value's field by its place (`about:<label>`, `safety:<n>`,
    /// `fingerprint:own` / `fingerprint:peer`), while drawn: the self-tests select in it.
    pub fn value_state(&self, key: &str) -> Option<Entity<InputState>> {
        self.values.get(key).cloned()
    }

    /// A `select-text` value (`Row`'s `dd`, the safety number, `Mono`): a read-only field of the
    /// text engine (drag, double and triple click, Ctrl+A, Ctrl+C), kept per place while shown.
    fn selectable(&mut self, key: SharedString, value: &str, multi_line: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let state = self
            .values
            .entry(key)
            .or_insert_with(|| {
                cx.new(|cx| if multi_line { InputState::multi_line(window, cx) } else { InputState::single_line(window, cx) }.read_only(true))
            })
            .clone();
        if state.read(cx).value() != value {
            state.update(cx, |state, cx| state.set_value(value, cx));
        }
        state
    }

    /// An About value: one line of selectable text in the row's look (`text-text-primary`, 13 px).
    fn value_text(&mut self, key: SharedString, value: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.selectable(key, value, false, window, cx);
        let style = value_style(FontRole::Sans, 13., None, cx);
        TextInput::new(&state, style).into_any_element()
    }

    /// `Mono`: an uppercase caption over a monospaced, selectable value that breaks anywhere.
    fn mono(&mut self, key: SharedString, label: impl Into<SharedString>, value: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.selectable(key, value, true, window, cx);
        let mut style = value_style(FontRole::Mono, 11., Some(0.06), cx);
        style.wrapping = decibell_text::Wrapping::MESSAGE;
        let t = cx.theme();
        let label: SharedString = label.into();
        div()
            .mt_2p5()
            .child(Label::new(label.to_uppercase()).size(10.).weight(600.).tracking(0.07).color(t.color_text_muted))
            .child(div().mt_1().child(TextArea::new(&state, style)))
            .into_any_element()
    }

    fn close(cx: &mut App) {
        let overlays = Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    /// `FriendActions`' click: the request; a failure toasts ("Friend request failed").
    fn act(&mut self, action: FriendAction, uid: Uid, name: SharedString, cx: &mut Context<Self>) {
        self.busy = true;
        cx.notify();
        let this = cx.weak_entity();
        let request = api::friends::send_friend_action(api::friends::SendFriendActionArgs {
            action: action as i32,
            target_username: name.to_string(),
            target_uid: Some(uid),
        });
        decibell_model::call::call(cx, request, move |result, cx| {
            if let Err(e) = result {
                let toasts = Model::global(cx).toasts.clone();
                toasts.update(cx, |t, cx| t.error("Friend request failed", Some(e.js_string()), cx));
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                cx.notify();
            });
        });
    }
}

/// "Settings → <tab>": replaces this modal (one modal at a time), as the renderer's.
fn open_settings(tab: &'static str, cx: &mut App) {
    let model = Model::global(cx).clone();
    model.nav.update(cx, |nav, cx| nav.set_settings_tab(tab, cx));
    model.overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
}

/// `Card`: `rounded-md border border-border-divider bg-bg-light px-4 py-3.5`, the title row with an
/// icon, a badge ("On" / "Off" / "Locked") and an action at its end.
fn card(title: SharedString, badge: Option<(&'static str, bool)>, icon: bool, action: Option<AnyElement>, body: Vec<AnyElement>, cx: &App) -> AnyElement {
    let t = cx.theme();
    div()
        .rounded(t.radius_md)
        .border_1()
        .border_color(t.color_border_divider)
        .bg(t.color_bg_light)
        .px_4()
        .py_3p5()
        .child(
            div()
                .mb_2p5()
                .flex()
                .items_center()
                .gap_2()
                .when(icon, |this| this.child(lock_glyph(14., t.color_text_secondary)))
                .child(div().min_w_0().child(Label::new(title).size(14.).weight(500.).color(t.color_text_primary).truncate()))
                .when_some(badge, |this, (text, on)| {
                    let color = if on { t.color_success } else { t.color_text_muted };
                    this.child(
                        div()
                            .rounded(t.radius_sm)
                            .px_1p5()
                            .py(px(1.))
                            .bg(color.opacity(0.15))
                            .child(Label::new(text.to_uppercase()).font(FontRole::Channel).size(10.).weight(600.).tracking(0.06).color(color)),
                    )
                })
                .when_some(action, |this, action| this.child(div().ml_auto().child(action))),
        )
        .children(body)
        .into_any_element()
}

fn para(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    Label::new(text).size(12.).line_height(12. * 1.55).color(cx.theme().color_text_muted).into_any_element()
}

/// A selectable value's look: a label's (the font at `size`, line height 1.5, `text-text-primary`,
/// `tracking` em or the body's) with Chromium's selection colours.
fn value_style(font: FontRole, size: f32, tracking: Option<f32>, cx: &mut App) -> InputStyle {
    let mut style = decibell_ui::TextField::input_style(font, size, 1.5, cx);
    if let Some(em) = tracking {
        style.text.letter_spacing = em * size;
    }
    style
}

const fn friend_label(status: FriendStatus) -> &'static str {
    match status {
        FriendStatus::Online | FriendStatus::Offline => "Friends",
        FriendStatus::PendingIncoming => "Wants to be friends",
        FriendStatus::PendingOutgoing => "Request sent",
        FriendStatus::Blocked => "Blocked",
    }
}

struct Shown {
    uid: Uid,
    own: bool,
    name: SharedString,
    friend: Option<Friend>,
    member: Option<Member>,
    roles: Vec<Role>,
    server: Option<ServerId>,
    server_name: Option<SharedString>,
    presence: decibell_model::live::presence::DisplayStatus,
}

impl ProfileModal {
    fn profile_tab(&mut self, s: &Shown, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let nickname = s.member.as_ref().map(|m| m.nickname.clone()).filter(|n| !n.is_empty() && *n != s.name);
        let mut rows: Vec<(SharedString, SharedString)> = vec![("Username".into(), s.name.clone())];
        if let Some(nickname) = nickname {
            rows.push(("Nickname".into(), nickname));
        }
        rows.push(("Status".into(), s.presence.label().into()));
        if !s.own {
            rows.push(("Friends".into(), s.friend.as_ref().map_or("Not friends", |f| friend_label(f.status)).into()));
        }
        if let Some(member) = s.member.as_ref().filter(|m| m.joined_at > 0) {
            let label = s.server_name.as_ref().map_or_else(|| "Joined server".into(), |n| format!("Joined {n}").into());
            rows.push((label, crate::fmt::short_date(member.joined_at)));
        }
        let muted = cx.theme().color_text_muted;
        // The values are `select-text` (`Row`'s `dd`).
        let values: Vec<AnyElement> = rows
            .iter()
            .map(|(l, v)| self.value_text(SharedString::from(format!("about:{l}")), v, window, cx))
            .collect();
        // `grid-cols-[max-content_1fr] gap-x-6 gap-y-2`: a label column as wide as its widest.
        let about = div()
            .flex()
            .gap_6()
            .child(div().flex().flex_col().flex_none().gap_2().children(rows.iter().map(|(l, _)| Label::new(l.clone()).size(13.).color(muted))))
            .child(div().flex().flex_col().flex_1().min_w_0().gap_2().children(values))
            .into_any_element();
        let mut cards = vec![card("About".into(), None, false, None, vec![about], cx)];
        if s.server.is_some() {
            let title: SharedString = s.server_name.as_ref().map_or_else(|| "Roles".into(), |n| format!("Roles in {n}").into());
            let chips = role_chips(&s.roles, s.member.as_ref().map_or(&[][..], |m| &m.role_ids[..]), s.member.as_ref().is_some_and(|m| m.is_owner), cx);
            cards.push(card(title, None, false, None, vec![chips.into_any_element()], cx));
        }
        div().flex().flex_col().gap_3().children(cards).into_any_element()
    }

    fn privacy_tab(&mut self, s: &Shown, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let model = Model::global(cx).clone();
        let (supported, status, fingerprint) = {
            let e2ee = model.live.e2ee.read(cx);
            (e2ee.supported, e2ee.status, e2ee.fingerprint.clone())
        };
        let secondary = Look::secondary(cx);
        let settings = move |label: &'static str, tab: &'static str| {
            div()
                .mt_3()
                .flex()
                .justify_end()
                .child(button(SharedString::from(format!("profile-{tab}-{label}")), label, secondary, false).on_click(move |_: &ClickEvent, _, cx| open_settings(tab, cx)))
                .into_any_element()
        };
        if s.own {
            let (badge, text) = if !supported || status == E2eeStatus::Unavailable {
                // The core hasn't decided yet (keys still loading after sign-in).
                (None, "Checking this device's encryption keys…")
            } else if status == E2eeStatus::NotSetUp {
                (Some(("Off", false)), "Your direct messages aren't encrypted yet. Set up encryption in Settings to seal them on your device.")
            } else if status == E2eeStatus::Locked {
                (
                    Some(("Locked", false)),
                    "Your keys are backed up, but this device hasn't unlocked them. Enter your passphrase in Settings to read and send encrypted messages here.",
                )
            } else {
                (
                    Some(("On", true)),
                    "Messages with anyone who has also set up encryption are sealed on your device; the server only ever stores encrypted data. Others compare the fingerprint below against what their app shows for you.",
                )
            };
            let mut body = vec![para(text, cx)];
            if status == E2eeStatus::Ready && !fingerprint.is_empty() {
                body.push(self.mono("fingerprint:own".into(), "Your fingerprint", &fingerprint, window, cx));
            }
            if supported && status != E2eeStatus::Unavailable {
                body.push(settings("Manage in Settings", "privacy"));
            }
            return div().flex().flex_col().gap_3().child(card("End-to-end encryption".into(), badge, true, None, body, cx)).into_any_element();
        }
        let name = &s.name;
        if !supported || status == E2eeStatus::Unavailable {
            return card(
                "End-to-end encryption".into(),
                None,
                true,
                None,
                vec![para("This device is still loading its encryption keys, so there is no safety number to compare yet.", cx)],
                cx,
            );
        }
        if status != E2eeStatus::Ready {
            let locked = status == E2eeStatus::Locked;
            let text: SharedString = if locked {
                format!("Unlock your keys on this device to see the safety number you share with {name}.").into()
            } else {
                format!("Set up encryption to seal your messages with {name} and get a safety number you can compare.").into()
            };
            return card(
                "End-to-end encryption".into(),
                Some((if locked { "Locked" } else { "Off" }, false)),
                true,
                None,
                vec![para(text, cx), settings(if locked { "Unlock in Settings" } else { "Set up in Settings" }, "privacy")],
                cx,
            );
        }
        let info: Option<PeerInfo> = PeerInfos::existing(cx).and_then(|store| store.read(cx).get(s.uid, current_key(s.uid, cx)).cloned());
        let Some(info) = info else {
            return card("End-to-end encryption".into(), None, true, None, vec![para(format!("Checking {name}'s keys…"), cx)], cx);
        };
        if !info.has_keys {
            return card(
                "End-to-end encryption".into(),
                Some(("Off", false)),
                true,
                None,
                vec![para(format!("{name} hasn't set up encryption. Messages between you are sent in plaintext, and the conversation says so."), cx)],
                cx,
            );
        }
        // The safety number's groups and the fingerprint, selectable (`select-text`): made before
        // the theme is borrowed for the rest.
        let scale = window.scale_factor();
        let group_style = value_style(FontRole::Mono, 14., Some(0.08), cx);
        let engine = decibell_text::Engine::global(cx);
        let groups_shown: Vec<AnyElement> = info
            .safety_number
            .split(' ')
            .enumerate()
            .map(|(i, group)| {
                let state = self.selectable(SharedString::from(format!("safety:{i}")), group, false, window, cx);
                // The field as wide as its text, centred in its column (`text-center`).
                let width = engine.label_layout(group, &group_style.text, decibell_text::Wrapping::NOWRAP, f32::INFINITY, scale).width();
                div().flex().justify_center().child(div().w(px((width * scale).ceil() / scale + 1.)).child(TextInput::new(&state, group_style))).into_any_element()
            })
            .collect();
        let peer_fingerprint = self.mono("fingerprint:peer".into(), format!("{name}'s fingerprint"), &info.fingerprint, window, cx);
        let t = cx.theme();
        let mut first = vec![para(format!("Messages between you and {name} are sealed on your devices; the server only ever stores encrypted data."), cx)];
        if info.changed_at > 0 {
            first.push(
                div()
                    .mt_2p5()
                    .rounded(t.radius_sm)
                    .border_1()
                    .border_color(t.color_warning.opacity(0.4))
                    .bg(t.color_warning.opacity(0.1))
                    .px_3()
                    .py_2()
                    .child(
                        Label::new(format!(
                            "Keys changed {}. {name} reset their encryption or signed in from a new device. Compare the safety number again before you trust what they send.",
                            crate::fmt::date_time(info.changed_at, cx)
                        ))
                        .size(12.)
                        .line_height(18.)
                        .color(t.color_warning),
                    )
                    .into_any_element(),
            );
        }
        first.push(
            div()
                .mt_2p5()
                .child(para(
                    if info.pinned {
                        format!("Identity pinned on this device: messages to {name} are never sent unencrypted, and a different key is flagged.")
                    } else {
                        "Not pinned yet: the identity is pinned the first time an encrypted message passes between you.".to_string()
                    },
                    cx,
                ))
                .into_any_element(),
        );
        let safety = info.safety_number.clone();
        let (accent_bright, accent) = (t.color_accent_bright, t.color_accent);
        let copy = div()
            .id("copy-safety-number")
            .relative()
            .children(decibell_shell::probes::probe("copy-safety-number"))
            .cursor_pointer()
            .text_color(accent_bright)
            .hover(move |s| s.text_color(accent))
            .on_click(move |_: &ClickEvent, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(safety.to_string()));
                let toasts = Model::global(cx).toasts.clone();
                toasts.update(cx, |t, cx| t.success("Safety number copied", None, cx));
            })
            .child(Label::new("Copy").size(11.).weight(500.))
            .into_any_element();
        let groups = div()
            .mt_2p5()
            .grid()
            .grid_cols(4)
            .gap_x_4()
            .gap_y_1p5()
            .rounded(t.radius_sm)
            .bg(t.color_bg_dark)
            .px_4()
            .py_3()
            .children(groups_shown)
            .into_any_element();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(card("End-to-end encryption".into(), Some(("On", true)), true, None, first, cx))
            .child(card(
                "Safety number".into(),
                None,
                false,
                Some(copy),
                vec![
                    para(format!("Identical on both your screens. Compare it with {name} in person or on a call; a match means nobody is sitting between you."), cx),
                    groups,
                ],
                cx,
            ))
            .child(card(
                "Fingerprint".into(),
                None,
                false,
                None,
                vec![
                    para(format!("What {name}'s own app shows under Settings → Privacy. Comparing fingerprints is the same check as the safety number, one identity at a time."), cx),
                    peer_fingerprint,
                ],
                cx,
            ))
            .into_any_element()
    }
}

impl Render for ProfileModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open;
        let modal = Modal::new("user-profile", open).on_close(|_: &CloseRequest, _, cx| ProfileModal::close(cx));
        let Some((uid, server)) = self.shown else { return modal };
        let model = Model::global(cx).clone();
        let own = model.auth.read(cx).uid == uid;
        let friend = model.live.friends.read(cx).get(uid).cloned();
        let (member, roles, server_name) = match server.and_then(|s| model.community(s, cx)) {
            Some(c) => (
                c.roster.read(cx).member(uid).cloned(),
                c.roles.read(cx).list().to_vec(),
                Some(c.meta.read(cx).name.clone()).filter(|n| !n.is_empty()),
            ),
            None => (None, Vec::new(), None),
        };
        let server_name = server_name.or_else(|| server.and_then(|s| model.servers.read(cx).get(s).map(|sv| sv.name.clone())));
        let fallback: SharedString = member
            .as_ref()
            .map(|m| m.username.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| friend.as_ref().map(|f| f.username.clone()))
            .unwrap_or_else(|| decibell_common::UNKNOWN_USER.into());
        let name = decibell_common::user_name(uid, &fallback, cx);
        let display = decibell_common::display_name(server, uid, &name, cx);
        let presence = presence_of(uid, member.as_ref().is_some_and(|m| m.is_online), cx);
        let message = status_message_of(uid, cx);
        let shown = Shown { uid, own, name: name.clone(), friend: friend.clone(), member, roles, server, server_name, presence };
        let content = match self.tab {
            ProfileTab::Profile => self.profile_tab(&shown, window, cx),
            ProfileTab::Privacy => self.privacy_tab(&shown, window, cx),
        };
        // A pending request either way, or a block, is not a conversation yet.
        let can_message = uid > 0 && !own && friend.as_ref().is_none_or(|f| f.status.is_accepted());
        let busy = self.busy;
        let mut actions: Vec<AnyElement> = Vec::new();
        if own {
            actions.push(button("profile-edit", "Edit profile", Look::secondary(cx), false).on_click(|_: &ClickEvent, _, cx| open_settings("account", cx)).into_any_element());
        } else {
            if uid > 0 {
                let act = |id: &'static str, label: &'static str, action: FriendAction, primary: bool, cx: &mut Context<Self>| {
                    let name = name.clone();
                    let look = if primary { Look::primary(cx) } else { Look::secondary(cx) };
                    button(id, label, look, busy)
                        .relative()
                        .children(decibell_shell::probes::probe(id))
                        .when(!busy, |this| this.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.act(action, uid, name.clone(), cx))))
                        .into_any_element()
                };
                match friend.as_ref().map(|f| f.status) {
                    Some(FriendStatus::PendingIncoming) => {
                        actions.push(act("profile-decline", "Decline", FriendAction::Reject, false, cx));
                        actions.push(act("profile-accept", "Accept", FriendAction::Accept, true, cx));
                    }
                    Some(FriendStatus::PendingOutgoing) => actions.push(act("profile-cancel", "Cancel request", FriendAction::Remove, false, cx)),
                    Some(FriendStatus::Blocked) => actions.push(act("profile-unblock", "Unblock", FriendAction::Remove, false, cx)),
                    Some(FriendStatus::Online | FriendStatus::Offline) => actions.push(act("profile-remove", "Remove friend", FriendAction::Remove, false, cx)),
                    None => actions.push(act("profile-add", "Add friend", FriendAction::Add, false, cx)),
                }
            }
            if can_message {
                let name = name.clone();
                actions.push(
                    button("profile-message", "Message", Look::primary(cx), false)
                        .on_click(move |_: &ClickEvent, _, cx| {
                            actions::dm::open_dm(uid, name.clone(), cx);
                            ProfileModal::close(cx);
                        })
                        .into_any_element(),
                );
            }
        }
        let gradient = person_gradient(uid, &name, cx);
        let pill = status_pill(presence, FontRole::Sans, cx);
        let t = cx.theme();
        let (bg_dark, border, primary, muted, secondary, divider, accent) = (
            t.color_bg_dark,
            t.color_border,
            t.color_text_primary,
            t.color_text_muted,
            t.color_text_secondary,
            t.color_border_divider,
            t.color_accent,
        );
        let avatar_radius = t.avatar_radius(px(96.));
        let tab = self.tab;
        let tab_button = |id: &'static str, label: &'static str, which: ProfileTab, cx: &mut Context<Self>| {
            let active = tab == which;
            div()
                .id(id)
                .relative()
                .children(decibell_shell::probes::probe(id))
                .mb(px(-1.))
                .border_b_2()
                .border_color(if active { accent } else { gpui::transparent_black() })
                .px_3()
                .py_2p5()
                .cursor_pointer()
                .text_color(if active { primary } else { secondary })
                .hover(move |s| s.text_color(primary))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tab = which;
                    if which == ProfileTab::Privacy {
                        this.want_info(cx);
                    }
                    cx.notify();
                }))
                .child(Label::new(label).size(13.).weight(500.))
        };
        let tabs = div()
            .flex()
            .flex_none()
            .gap_1()
            .border_b_1()
            .border_color(divider)
            .px_6()
            .child(tab_button("profile-tab-profile", "Profile", ProfileTab::Profile, cx))
            .child(tab_button("profile-tab-privacy", "Privacy", ProfileTab::Privacy, cx));
        let t = cx.theme();
        let card = div()
            .id(("profile-screen", uid as u64))
            .occlude()
            .flex()
            .flex_col()
            .h(px(560.))
            .w(px(640.))
            .overflow_hidden()
            .rounded(t.radius_xl)
            .border_1()
            .border_color(border)
            .bg(bg_dark)
            .shadow(t.shadow_modal.clone())
            .child(
                div().relative().h(px(120.)).flex_none().bg(gradient).child(
                    // `bg-black/30 text-white/80 hover:bg-black/50 hover:text-white`: literals.
                    div()
                        .id("profile-close")
                        .absolute()
                        .right_3()
                        .top_3()
                        .flex()
                        .size_7()
                        .items_center()
                        .justify_center()
                        .rounded(t.radius_sm)
                        .bg(gpui::black().opacity(0.3))
                        .cursor_pointer()
                        .hover(|s| s.bg(gpui::black().opacity(0.5)))
                        .on_click(|_: &ClickEvent, _, cx| ProfileModal::close(cx))
                        .child(Icon::new(IconName::Close).size(px(14.)).color(gpui::white().opacity(0.8))),
                ),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .px_6()
                    .child(
                        // The 6 px ring cuts the avatar into the banner.
                        div()
                            .absolute()
                            .top(px(-48.))
                            .left(px(24.))
                            .p(px(6.))
                            .rounded(avatar_radius + px(6.))
                            .bg(bg_dark)
                            .child(
                                div()
                                    .relative()
                                    .size(px(96.))
                                    .child(UserAvatar::new(uid, name.clone(), 96.))
                                    .child(div().absolute().bottom(px(-2.)).right(px(-2.)).child(StatusDot::new(dot_presence(presence), bg_dark).size(24.).ring_width(4.))),
                            ),
                    )
                    .child(div().flex().min_h(px(56.)).items_start().justify_end().gap_2().pt_3().children(actions))
                    .child(
                        div()
                            .pb_4()
                            .pt_1()
                            .child(Label::new(display.clone()).font(FontRole::Display).size(20.).line_height(25.).weight(600.).color(primary))
                            .when(display != name, |this| this.child(div().mt_0p5().child(Label::new(name.clone()).size(13.).color(muted))))
                            .child(div().mt_2().flex().child(pill))
                            .when_some(message, |this, message| {
                                this.child(div().mt_2p5().child(StatusMessageText::new(message).font(FontRole::Sans).size(14.).line_height(14. * 1.375).color(secondary).emoji_size(18.).wrap()))
                            }),
                    ),
            )
            .child(tabs)
            .child(ScrollArea::new(("profile-content", uid as u64)).flex_1().child(div().px_6().py_5().child(content)));
        modal.child(card)
    }
}
