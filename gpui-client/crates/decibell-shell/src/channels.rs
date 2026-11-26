//! The server-mode sidebar (`features/channels/ServerChannelsSidebar.tsx`,
//! `features/voice/VoiceParticipantList.tsx`, `features/servers/ServerActionsDropdown.tsx`): the
//! community's header and menu, then one flat ordered list — uncategorized channels, then
//! category blocks (collapsible) — of text channels and voice channels with who is in them.
//!
//! Kept current by keyed events: a voice roster change in another community, a speaking flip of
//! someone not shown, a roster delta that changes no permission or nickname shown here: none
//! re-render it.

use std::collections::HashSet;
use std::rc::Rc;

use decibell_common::{BoundsCell, UserAvatar, Watch, display_name, open_profile_beside, open_user_menu};
use decibell_core::api;
use decibell_model::live::voice::gain;
use decibell_model::live::voice::rosters::{VoiceRosterEvent, VoiceUserState};
use decibell_model::live::voice::speaking::SpeakingChanged;
use decibell_model::live::voice::streams::StreamsChanged;
use decibell_model::stores::community::{Channel, ChannelKind, Community};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::{has_bits, perm};
use decibell_model::{ChannelId, Model, ServerId, Uid, actions, call::call_or_toast};
use decibell_ui::{
    ActiveTheme as _, ConfirmModal, ConfirmTone, ContextMenu, DismissEvent, FontRole, Icon, IconName, Label, MenuEntry,
    MenuItem, ScrollArea, ScrollHandle, title_tooltip,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, DragMoveEvent, Entity, Hsla, IntoElement, MouseButton, MouseDownEvent, Pixels,
    Point, Render, SharedString, Subscription, Window, div, point, prelude::*, px,
};

use crate::ink::Ink;
use crate::requests::{ShellRequests, SidebarMenu, create_channel};
use crate::sidebar::{ResizeDrag, resize_handle, sidebar_frame};
use crate::slots::{ChannelMenuTarget, ChannelReorder, channel_menu_extensions, channel_reorder};
use crate::user_panel::PanelHeight;

/// A voice member being dragged onto another voice channel (`memberDrag.ts`).
#[derive(Clone)]
pub struct MemberDrag {
    server: ServerId,
    from: ChannelId,
    uid: Uid,
    name: SharedString,
}

/// What the sidebar shows of the stores it doesn't redraw for wholesale.
#[derive(Default, PartialEq)]
struct Shown {
    /// The local user's permissions that gate entries here.
    manage_channels: bool,
    invite: bool,
    /// Nicknames of the people in the voice lists.
    nicknames: Vec<(Uid, SharedString)>,
}

pub struct ChannelSidebar {
    model: Model,
    nav: (ActiveView, Option<ServerId>, Option<ChannelId>),
    collapsed: HashSet<ChannelId>,
    server_menu: bool,
    context_menu: Option<(Point<Pixels>, ChannelMenuTarget)>,
    delete: Option<(ChannelId, SharedString, bool)>,
    shown: Shown,
    /// People drawn in the voice lists (keyed events filter on them).
    people: HashSet<Uid>,
    /// Whose speaking ring this sidebar draws: the connected channel's participants.
    speakers: HashSet<Uid>,
    scroll: ScrollHandle,
    header_bounds: BoundsCell,
    panel: Entity<PanelHeight>,
    resize: ResizeDrag,
    /// The active community's own subscriptions (replaced when it changes).
    community: Option<(ServerId, Vec<Subscription>)>,
    /// Drag to reorder, when a feature registered it (`slots::ChannelReorder`).
    reorder: Option<Rc<dyn ChannelReorder>>,
    _subscriptions: Vec<Subscription>,
    _watch: Watch,
}

impl ChannelSidebar {
    pub fn new(panel: Entity<PanelHeight>, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.nav, cx, |this, nav, _| {
                let next = (nav.active_view, nav.active_server, nav.active_channel.clone());
                std::mem::replace(&mut this.nav, next.clone()) != next
            })
            .store(&model.communities, cx)
            .store(&model.servers, cx)
            .store(&model.settings, cx)
            .store(&panel, cx)
            .store_if(&model.layout, cx, |this, layout, _| this.resize.width_changed(layout.prefs().sidebar_width as f32))
            .store(&voice.session, cx)
            .store(&voice.participants, cx)
            .event(&voice.rosters, cx, |this, e: &VoiceRosterEvent| {
                let VoiceRosterEvent::Changed(keys) = e;
                keys.iter().any(|(server, _)| Some(*server) == this.nav.1)
            })
            .event(&voice.streams, cx, |this, e: &StreamsChanged| {
                e.key.as_ref().is_some_and(|(server, _)| Some(*server) == this.nav.1)
            })
            .event(&voice.speaking, cx, |this, e: &SpeakingChanged| this.speakers.contains(&e.uid))
            .people(cx, |this, uid| this.people.contains(&uid));
        let nav = model.nav.read(cx);
        let nav = (nav.active_view, nav.active_server, nav.active_channel.clone());
        let width = model.layout.read(cx).prefs().sidebar_width as f32;
        let reorder = channel_reorder(cx);
        let mut subscriptions = Vec::new();
        if let Some(reorder) = &reorder {
            let weak = cx.weak_entity();
            subscriptions.push(reorder.subscribe(
                Box::new(move |cx| {
                    let _ = weak.update(cx, |_, cx| cx.notify());
                }),
                cx,
            ));
        }
        // A menu asked for from outside (the screens): taken now and whenever one is asked for.
        let requests = ShellRequests::entity(cx);
        subscriptions.push(cx.observe(&requests, |this: &mut Self, requests, cx| this.take_menu_request(&requests, cx)));
        let mut this = Self {
            model,
            nav,
            collapsed: HashSet::new(),
            server_menu: false,
            context_menu: None,
            delete: None,
            shown: Shown::default(),
            people: HashSet::new(),
            speakers: HashSet::new(),
            scroll: ScrollHandle::new(),
            header_bounds: BoundsCell::new(),
            panel,
            resize: ResizeDrag::new(width),
            community: None,
            reorder,
            _subscriptions: subscriptions,
            _watch: watch,
        };
        this.take_menu_request(&requests, cx);
        this
    }

    fn take_menu_request(&mut self, requests: &Entity<ShellRequests>, cx: &mut Context<Self>) {
        let Some(menu) = requests.update(cx, |r, _| r.sidebar_menu.take()) else { return };
        match menu {
            SidebarMenu::Server => {
                self.server_menu = true;
                self.context_menu = None;
            }
            SidebarMenu::Channel(at, target) => self.context_menu = Some((at, target)),
        }
        cx.notify();
    }

    /// Follow the active community's own entities (its channels, meta, roster) while it is shown.
    fn follow(&mut self, community: Option<&Community>, cx: &mut Context<Self>) {
        let id = community.map(|c| c.id);
        if self.community.as_ref().map(|(id, _)| *id) == id {
            return;
        }
        self.community = community.map(|c| {
            let subscriptions = vec![
                cx.observe(&c.channels, |_, _, cx| cx.notify()),
                cx.observe(&c.meta, |_, _, cx| cx.notify()),
                cx.observe(&c.roles, |_, _, cx| cx.notify()),
                // Every presence flip of every member: redraw only when a permission or a
                // nickname shown here moved.
                cx.observe(&c.roster, |this, _, cx| {
                    let shown = this.compute_shown(cx);
                    if shown != this.shown {
                        this.shown = shown;
                        cx.notify();
                    }
                }),
            ];
            (c.id, subscriptions)
        });
        self.shown = self.compute_shown(cx);
    }

    fn compute_shown(&self, cx: &App) -> Shown {
        let Some(server) = self.nav.1 else { return Shown::default() };
        let model = &self.model;
        let mut nicknames: Vec<(Uid, SharedString)> = Vec::new();
        if let Some(community) = model.community(server, cx) {
            let roster = community.roster.read(cx);
            for uid in &self.people {
                if let Some(nick) = roster.nickname(*uid) {
                    nicknames.push((*uid, nick.clone()));
                }
            }
        }
        nicknames.sort();
        Shown {
            manage_channels: model.has_permission(server, perm::MANAGE_CHANNELS, cx),
            invite: model.has_permission(server, perm::CREATE_INVITE, cx)
                || model.has_permission(server, perm::MANAGE_INVITES, cx),
            nicknames,
        }
    }

    fn toggle_category(&mut self, id: ChannelId, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        cx.notify();
    }

    fn open_context_menu(&mut self, at: Point<Pixels>, target: ChannelMenuTarget, cx: &mut Context<Self>) {
        self.context_menu = Some((at, target));
        self.server_menu = false;
        cx.notify();
    }

    fn close_menus(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() | std::mem::take(&mut self.server_menu) {
            cx.notify();
        }
    }
}

/// `list-row` metrics of the channel list (`--list-row-pad-y: 7px; --list-row-pad-x: 10px;
/// --list-row-gap: 8px`) at the list density.
fn channel_row(id: impl Into<gpui::ElementId>, scale: f32, t: &Ink) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .w_full()
        .items_center()
        .py(px(7. * scale))
        .px(px(10. * scale))
        .gap(px(8. * scale))
        .rounded(t.radius_sm)
        .cursor_pointer()
}

/// The `.channel-icon` slot: a square of 1.45 em of the row's text size, the `#` set at that
/// size (weight 500, line-height 1), the speaker at 0.82 em of it.
fn channel_icon(text_size: f32, color: Hsla, voice: bool) -> AnyElement {
    let slot = (text_size * 1.45).round();
    let inner = if voice {
        Icon::new(IconName::SettingsSpeaker).size(px(slot * 0.82)).color(color).into_any_element()
    } else {
        Label::new("#").font(FontRole::Channel).size(slot).line_height(slot).weight(500.).color(color).into_any_element()
    };
    div().flex().flex_none().size(px(slot)).items_center().justify_center().child(inner).into_any_element()
}

/// A participant's badges: LIVE, muted by us, server mute / deafen / no speak, deafened / muted.
fn participant_badges(live: bool, local_mute: bool, state: VoiceUserState, t: &Ink) -> impl IntoElement + use<> {
    let server_badge = (state.server_muted || state.server_deafened || state.suppressed).then(|| {
        let (text, title) = if state.server_deafened {
            ("SRV DEAF", "Server deafened by a moderator")
        } else if !state.server_muted && state.suppressed {
            ("NO SPEAK", "Can't speak in this channel")
        } else {
            ("SRV MUTE", "Server muted by a moderator")
        };
        div()
            .id(title)
            .rounded(t.radius_sm)
            .bg(t.color_error.opacity(0.15))
            .px_1()
            .py(px(1.))
            .tooltip(title_tooltip(title))
            .child(Label::new(text).size(9.).weight(600.).tracking(0.04).color(t.color_error))
    });
    div()
        .ml_auto()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .when(live, |this| {
            this.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(t.radius_sm)
                    .bg(t.color_error.opacity(0.2))
                    .px_1p5()
                    .py_0p5()
                    .child(div().size_1p5().rounded_full().bg(t.color_error))
                    .child(Label::new("LIVE").size(10.).weight(600.).color(t.color_error)),
            )
        })
        .when(local_mute, |this| this.child(Icon::new(IconName::VoiceLocalMute).size(px(14.)).color(t.color_accent)))
        .children(server_badge)
        .map(|this| {
            if state.deafened {
                this.child(Icon::new(IconName::VoiceDeafen).size(px(14.)).color(t.color_error))
            } else if state.muted {
                this.child(Icon::new(IconName::VoiceMute).size(px(14.)).color(t.color_error))
            } else {
                this
            }
        })
}

/// What the drag of a voice member shows under the pointer.
struct DragGhost {
    name: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::ink::chrome(cx);
        div()
            .rounded(t.radius_sm)
            .bg(t.color_bg_light)
            .border_1()
            .border_color(t.color_border)
            .px_2()
            .py_1()
            .shadow(cx.theme().chrome.shadow_float.clone())
            .child(Label::new(self.name.clone()).size(cx.theme().scaled.text_member).color(t.color_text_primary))
    }
}

impl Render for ChannelSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let t = crate::ink::chrome(cx);
        let (scale, text_channel, text_member, text_section, text_title, text_micro) = {
            let s = &cx.theme().scaled;
            (s.list_row_avatar_scale, s.text_channel, s.text_member, s.text_section, s.text_title, s.text_micro)
        };
        let (emphasis, tracking_title, tracking_section) = {
            let p = cx.theme().palette;
            (p.font_weight_emphasis, p.tracking_title, p.tracking_section)
        };
        let (view, server, active_channel) = self.nav.clone();
        let community = server.and_then(|id| model.community(id, cx));
        self.follow(community.as_ref(), cx);
        let me = model.auth.read(cx).uid;

        let server_name: SharedString = server
            .and_then(|id| {
                community
                    .as_ref()
                    .map(|c| c.meta.read(cx).name.clone())
                    .filter(|n| !n.is_empty())
                    .or_else(|| model.servers.read(cx).get(id).map(|s| s.name.clone()))
            })
            .unwrap_or_else(|| "Server".into());
        // The renderer's test (`servers.some(s => s.id === activeServerId)`) is against the
        // memberships, so a joined community always reads "Public". Mirrored.
        let public = server.is_some_and(|id| model.servers.read(cx).get(id).is_some());

        let channels: Vec<Channel> = community.as_ref().map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
        let context_menu = self.context_menu.clone().map(|(at, target)| channel_context_menu(at, target, &channels, cx));
        let mut wanted: Vec<(ServerId, Uid)> = Vec::new();
        // Drag to reorder: rows are draggable for MANAGE_CHANNELS holders (`onListDragStart`).
        let reorder = self.reorder.clone().filter(|_| self.shown.manage_channels && server.is_some());
        if let (Some(reorder), Some(server)) = (&reorder, server) {
            reorder.begin(server, cx);
        }
        let list_scroll = self.scroll.clone();
        // A press that ended a drag is no click.
        let swallowed = {
            let reorder = reorder.clone();
            Rc::new(move |cx: &App| reorder.as_ref().is_some_and(|r| r.swallow_click(cx)))
        };
        let cxr: &Context<Self> = cx;
        let is_owner = server.is_some_and(|id| model.is_owner(id, cx));
        let manage_channels = self.shown.manage_channels;
        let row_manageable = |c: &Channel| {
            manage_channels
                || (c.my_permissions != 0
                    && (has_bits(c.my_permissions, perm::MANAGE_CHANNELS) || has_bits(c.my_permissions, perm::MANAGE_ROLES)))
        };

        // Voice: the connected channel (only when it is in this community), the rosters, the
        // participants, speaking, streams, local mutes.
        let voice = model.live.voice.clone();
        let connected: Option<ChannelId> = voice
            .session
            .read(cx)
            .connected()
            .filter(|(s, _)| Some(*s) == server)
            .map(|(_, c)| c);
        let rosters = voice.rosters.read(cx);
        let streams = voice.streams.read(cx);
        let speaking = voice.speaking.read(cx);
        let settings = model.settings.read(cx).get().clone();
        let participants = voice.participants.read(cx).list().to_vec();
        let can_move_in = |channel: &str| server.is_some_and(|s| is_owner || model.channel_permission(s, channel, perm::MOVE_MEMBERS, cx));

        let mut people = HashSet::new();
        let mut speakers = HashSet::new();
        let mut rows: Vec<AnyElement> = Vec::new();

        let mut voice_row = |c: &Channel, rows: &mut Vec<AnyElement>| {
            let cx = cxr;
            let Some(server) = server else { return };
            let is_connected = connected.as_deref() == Some(&*c.id);
            let (bg, fg, icon, weight) = if is_connected && view == ActiveView::Voice {
                (Some(t.color_accent_soft), t.color_text_bright, t.color_accent, 600.)
            } else if is_connected {
                (None, t.color_success, t.color_success, 600.)
            } else {
                (None, t.color_text_secondary, t.color_text_muted, 400.)
            };
            let channel_id = c.id.clone();
            let manageable = row_manageable(c);
            let group: SharedString = format!("voice-row-{}", c.id).into();
            let drop_target = c.id.clone();
            let row = channel_row(gpui::ElementId::Name(c.id.clone()), scale, &t)
                .group(group.clone())
                .when_some(bg, |this, bg| this.bg(bg))
                .when(bg.is_none(), |this| this.hover(|s| s.bg(t.color_surface_hover)))
                .children(crate::probes::probe(format!("voice:{}", c.id)))
                .when_some(reorder.clone(), |this, reorder| {
                    let (id, list) = (channel_id.clone(), list_scroll.clone());
                    this.children(reorder.row_extras(&c.id, cx)).on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                        reorder.press(server, id.clone(), e.position, &list, cx)
                    })
                })
                .on_click({
                    let id = channel_id.clone();
                    let swallowed = swallowed.clone();
                    move |_: &ClickEvent, _, cx| {
                        if !swallowed(cx) {
                            actions::voice::click_voice_channel(server, id.clone(), cx)
                        }
                    }
                })
                .on_mouse_down(MouseButton::Right, cx.listener({
                    let id = channel_id.clone();
                    move |this, e: &MouseDownEvent, _, cx| {
                        if this.shown.manage_channels {
                            cx.stop_propagation();
                            this.open_context_menu(e.position, ChannelMenuTarget::Channel { server, channel: id.clone() }, cx);
                        }
                    }
                }))
                // A voice member dragged here moves them (MOVE_MEMBERS in their channel; the
                // server checks the rest and refuses through `mod_action_responded`).
                .drag_over::<MemberDrag>({
                    let target = drop_target.clone();
                    let (accent, on_accent) = (t.color_accent, t.color_on_accent);
                    move |style, drag: &MemberDrag, _, _| {
                        if drag.server == server && drag.from != target { style.bg(accent).text_color(on_accent) } else { style }
                    }
                })
                .on_drop::<MemberDrag>({
                    let target = drop_target.clone();
                    move |drag: &MemberDrag, _, cx| {
                        if drag.server == server && drag.from != target {
                            actions::voice::voice_mod(server, drag.uid, "move", Some(target.clone()), cx);
                        }
                    }
                })
                .child(channel_icon(text_channel, icon, true))
                .child(
                    div().min_w_0().text_color(fg).child(
                        Label::new(c.name.clone()).font(FontRole::Channel).size(text_channel).weight(weight).truncate(),
                    ),
                )
                .when(manageable, |this| this.child(settings_gear(&group, channel_id.clone(), &t)));
            rows.push(row.into_any_element());

            // Who is in it: the live participants when we are, else the community's roster.
            let list: Vec<(Uid, SharedString, VoiceUserState)> = if is_connected {
                participants
                    .iter()
                    .map(|p| {
                        let state = if p.uid == me {
                            let s = voice.session.read(cx);
                            VoiceUserState { muted: s.muted, deafened: s.deafened, ..p.state }
                        } else {
                            p.state
                        };
                        (p.uid, p.name.clone(), state)
                    })
                    .collect()
            } else {
                let key = (server, c.id.clone());
                match rosters.channel(&key) {
                    Some(roster) => roster.roster.iter().map(|uid| (*uid, SharedString::default(), roster.state(*uid))).collect(),
                    None => Vec::new(),
                }
            };
            if list.is_empty() {
                return;
            }
            let movable = can_move_in(&c.id);
            let mut items = Vec::with_capacity(list.len());
            for (uid, fallback, state) in list {
                people.insert(uid);
                let is_speaking = is_connected && speaking.is_speaking(uid);
                if is_connected {
                    speakers.insert(uid);
                }
                let live = streams.of_user(uid).is_some_and(|l| l.server == server && l.channel == c.id);
                let local_mute = gain::locally_muted(&settings, uid);
                let username = decibell_common::user_name(uid, &fallback, cx);
                let name = display_name(Some(server), uid, &username, cx);
                if model.community(server, cx).is_some_and(|c| c.roster.read(cx).member(uid).is_none()) {
                    wanted.push((server, uid));
                }
                let bounds = BoundsCell::new();
                let group: SharedString = format!("voice-member-{}-{uid}", c.id).into();
                let drag = MemberDrag { server, from: c.id.clone(), uid, name: name.clone() };
                let row = div()
                    .id(("voice-member", uid as u64))
                    .group(group.clone())
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(t.radius_sm)
                    .px_1p5()
                    .py_1()
                    .hover(|s| s.bg(t.color_surface_hover))
                    .cursor_pointer()
                    .child(bounds.probe())
                    .on_click({
                        let bounds = bounds.clone();
                        move |_: &ClickEvent, _, cx| open_profile_beside(uid, bounds.get(), Some(server), cx)
                    })
                    .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        open_user_menu(uid, e.position, Some(server), cx);
                    })
                    .when(movable && uid != me && uid > 0, |this| {
                        this.on_drag(drag, |drag: &MemberDrag, _, _, cx| cx.new(|_| DragGhost { name: drag.name.clone() }))
                    })
                    .child(
                        div()
                            .flex_none()
                            .rounded(t.radius_sm)
                            // The speaking ring flips instantly, as in the renderer.
                            .when(is_speaking, |this| {
                                this.shadow(vec![
                                    gpui::BoxShadow {
                                        color: t.color_success,
                                        offset: point(px(0.), px(0.)),
                                        blur_radius: px(0.),
                                        spread_radius: px(2.),
                                        inset: false,
                                    },
                                    gpui::BoxShadow {
                                        color: t.color_success,
                                        offset: point(px(0.), px(0.)),
                                        blur_radius: px(3.),
                                        spread_radius: px(0.),
                                        inset: false,
                                    },
                                ])
                            })
                            // Not a `.list-row`: the avatar keeps its size at every list density.
                            .child(UserAvatar::new(uid, username.clone(), 22.).chrome()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_color(if is_speaking { t.color_success } else { t.color_text_secondary })
                            .when(!is_speaking, |this| {
                                let primary = t.color_text_primary;
                                this.group_hover(group.clone(), move |s| s.text_color(primary))
                            })
                            .child(Label::new(name).font(FontRole::Channel).size(text_member).truncate()),
                    )
                    .child(participant_badges(live, local_mute, state, &t));
                items.push(row.into_any_element());
            }
            rows.push(div().flex().flex_col().gap_0p5().pb_1().pl_5().children(items).into_any_element());
        };

        let text_row = |c: &Channel, rows: &mut Vec<AnyElement>| {
            let cx = cxr;
            let Some(server) = server else { return };
            let active = active_channel.as_deref() == Some(&*c.id) && view == ActiveView::Server;
            let group: SharedString = format!("text-row-{}", c.id).into();
            let id = c.id.clone();
            let manageable = row_manageable(c);
            let row = channel_row(gpui::ElementId::Name(c.id.clone()), scale, &t)
                .group(group.clone())
                .map(|this| if active { this.bg(t.color_accent_soft) } else { this.hover(|s| s.bg(t.color_surface_hover)) })
                .children(crate::probes::probe(format!("channel:{}", c.id)))
                .when_some(reorder.clone(), |this, reorder| {
                    let (id, list) = (id.clone(), list_scroll.clone());
                    this.children(reorder.row_extras(&c.id, cx)).on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                        reorder.press(server, id.clone(), e.position, &list, cx)
                    })
                })
                .on_click({
                    let id = id.clone();
                    let swallowed = swallowed.clone();
                    move |_: &ClickEvent, _, cx| {
                        if swallowed(cx) {
                            return;
                        }
                        let nav = Model::global(cx).nav.clone();
                        nav.update(cx, |nav, cx| {
                            if nav.active_channel.as_ref() != Some(&id) {
                                nav.set_active_channel(Some(id.clone()), cx);
                            }
                            nav.set_active_view(ActiveView::Server, cx);
                        });
                    }
                })
                .on_mouse_down(MouseButton::Right, cx.listener({
                    let id = id.clone();
                    move |this, e: &MouseDownEvent, _, cx| {
                        if this.shown.manage_channels {
                            cx.stop_propagation();
                            this.open_context_menu(e.position, ChannelMenuTarget::Channel { server, channel: id.clone() }, cx);
                        }
                    }
                }))
                .child(channel_icon(text_channel, if active { t.color_accent_bright } else { t.color_text_muted }, false))
                .child(
                    div()
                        .min_w_0()
                        .text_color(if active { t.color_text_bright } else { t.color_text_secondary })
                        .when(!active, |this| {
                            let primary = t.color_text_primary;
                            this.group_hover(group.clone(), move |s| s.text_color(primary))
                        })
                        .child(
                            Label::new(c.name.clone())
                                .font(FontRole::Channel)
                                .size(text_channel)
                                .weight(if active { 600. } else { 400. })
                                .truncate(),
                        ),
                )
                .when(c.encrypted, |this| {
                    this.child(
                        div()
                            .id("encrypted")
                            .flex_none()
                            .tooltip(title_tooltip("End-to-end encrypted channel"))
                            .child(Icon::new(IconName::ChatLock).size(px(11.)).color(t.color_text_muted)),
                    )
                })
                .when(manageable, |this| this.child(settings_gear(&group, id.clone(), &t)));
            rows.push(row.into_any_element());
        };

        // Flat Discord-style grouping: a channel belongs to the nearest category above it.
        let mut category: Option<(Channel, bool)> = None;
        for c in &channels {
            match c.kind {
                ChannelKind::Category => {
                    let collapsed = self.collapsed.contains(&c.id);
                    category = Some((c.clone(), collapsed));
                    let Some(server) = server else { continue };
                    let group: SharedString = format!("category-{}", c.id).into();
                    let id = c.id.clone();
                    // `mt-3 first:mt-0` on the block: only the list's first child has no margin
                    // (uncategorized channels above it give the first block its margin too).
                    let first_child = rows.is_empty();
                    rows.push(
                        div()
                            .id(gpui::ElementId::Name(c.id.clone()))
                            .group(group.clone())
                            .relative()
                            .children(crate::probes::probe(format!("category:{}", c.id)))
                            .when_some(reorder.clone(), |this, reorder| {
                                let (id, list) = (id.clone(), list_scroll.clone());
                                this.children(reorder.row_extras(&c.id, cx)).on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                                    reorder.press(server, id.clone(), e.position, &list, cx)
                                })
                            })
                            .when(!first_child, |this| this.mt_3())
                            .mb_0p5()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .on_click(cx.listener({
                                let id = id.clone();
                                let swallowed = swallowed.clone();
                                move |this, _: &ClickEvent, _, cx| {
                                    if !swallowed(cx) {
                                        this.toggle_category(id.clone(), cx)
                                    }
                                }
                            }))
                            .on_mouse_down(MouseButton::Right, cx.listener({
                                let id = id.clone();
                                move |this, e: &MouseDownEvent, _, cx| {
                                    if this.shown.manage_channels {
                                        cx.stop_propagation();
                                        this.open_context_menu(e.position, ChannelMenuTarget::Category { server, channel: id.clone() }, cx);
                                    }
                                }
                            }))
                            .child(
                                gpui::svg()
                                    .path("icons/settings/chevron-down.svg")
                                    .size(px(10.))
                                    .flex_none()
                                    .text_color(t.color_text_muted)
                                    .with_transformation(gpui::Transformation::rotate(gpui::radians(if collapsed {
                                        -std::f32::consts::FRAC_PI_2
                                    } else {
                                        0.
                                    }))),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_color(t.color_text_muted)
                                    .group_hover(group.clone(), {
                                        let c = t.color_text_secondary;
                                        move |s| s.text_color(c)
                                    })
                                    .child(
                                        Label::new(SharedString::from(c.name.to_uppercase()))
                                            .font(FontRole::Section)
                                            .size(text_section)
                                            .line_height(text_section)
                                            .weight(500.)
                                            .tracking(tracking_section)
                                            .truncate(),
                                    ),
                            )
                            .when(manage_channels, |this| {
                                this.child(
                                    div()
                                        .id("create-in-category")
                                        .group("create-in-category")
                                        .ml_auto()
                                        .flex()
                                        .flex_none()
                                        .size_4()
                                        .items_center()
                                        .justify_center()
                                        .rounded(t.radius_sm)
                                        .invisible()
                                        .group_hover(group.clone(), |s| s.visible())
                                        .hover(|s| s.bg(t.color_surface_hover))
                                        .tooltip(title_tooltip("Create channel in category"))
                                        .on_click({
                                            let id = id.clone();
                                            move |_: &ClickEvent, _, cx| {
                                                cx.stop_propagation();
                                                create_channel(server, ChannelKind::Text, Some(id.clone()), true, cx);
                                            }
                                        })
                                        .child(Icon::new(IconName::SettingsPlus).size(px(11.)).stroke_width(2.5).color(t.color_text_muted)),
                                )
                            })
                            .into_any_element(),
                    );
                }
                ChannelKind::Voice | ChannelKind::Text | ChannelKind::Unknown => {
                    if category.as_ref().is_some_and(|(_, collapsed)| *collapsed) {
                        continue;
                    }
                    if c.kind == ChannelKind::Voice {
                        voice_row(c, &mut rows);
                    } else {
                        text_row(c, &mut rows);
                    }
                }
            }
        }
        drop(voice_row);
        for (server, uid) in wanted {
            decibell_common::want_member(server, uid, cx);
        }
        self.people = people;
        self.speakers = speakers;
        let shown = self.compute_shown(cx);
        if shown != self.shown {
            self.shown = shown;
        }
        let empty = channels.is_empty();

        let menu_open = self.server_menu;
        crate::probes::flag("server-menu-open", menu_open && server.is_some());
        crate::probes::flag("channel-menu-open", self.context_menu.is_some());
        let header = div()
            .relative()
            .flex()
            .flex_none()
            .h_12()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(t.color_border)
            .px_4()
            .child(self.header_bounds.probe())
            .child(
                div()
                    .id("server-menu-button")
                    .relative()
                    .children(crate::probes::probe("server-menu-button"))
                    .ml(px(-8.))
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .rounded(t.radius_md)
                    .px_2()
                    .py_1p5()
                    .when(server.is_some(), |this| {
                        this.cursor_pointer()
                            .hover(|s| s.bg(t.color_surface_hover))
                            .tooltip(title_tooltip("Server options"))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.server_menu = !this.server_menu;
                                this.context_menu = None;
                                cx.notify();
                            }))
                    })
                    .child(
                        Label::new(server_name)
                            .font(FontRole::Display)
                            .size(text_title)
                            .weight(emphasis)
                            .tracking(tracking_title)
                            .color(t.color_text_bright)
                            .truncate(),
                    )
                    .when(server.is_some(), |this| {
                        this.child(
                            gpui::svg()
                                .path("icons/settings/chevron-down.svg")
                                .size(px(12.))
                                .flex_none()
                                .text_color(t.color_text_muted)
                                .with_transformation(gpui::Transformation::rotate(gpui::radians(if menu_open {
                                    std::f32::consts::PI
                                } else {
                                    0.
                                }))),
                        )
                    }),
            )
            .child(
                div()
                    .ml_auto()
                    .rounded(t.radius_sm)
                    .bg(if public { t.color_success.opacity(0.15) } else { t.color_text_muted.opacity(0.15) })
                    .px_1p5()
                    .py_0p5()
                    .child(
                        Label::new(if public { "PUBLIC" } else { "PRIVATE" })
                            .font(FontRole::Meta)
                            .size(text_micro)
                            .line_height(text_micro)
                            .weight(500.)
                            .tracking(0.1)
                            .color(if public { t.color_success } else { t.color_text_secondary }),
                    ),
            )
            .when(menu_open && server.is_some(), |this| {
                this.child(server_menu(server.unwrap(), self.shown.invite, self.header_bounds.clone(), cx))
            });

        let list = ScrollArea::new("channel-list")
            .track_scroll(&self.scroll)
            .flex_1()
            .child(
                div()
                    .id("channel-list-content")
                    .px_2()
                    .py_2p5()
                    .min_h_full()
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        if let Some(server) = server
                            && this.shown.manage_channels
                        {
                            this.open_context_menu(e.position, ChannelMenuTarget::Empty { server }, cx);
                        }
                    }))
                    .children(rows)
                    // `dropBefore === "END"`: the line under the last row.
                    .when_some(reorder.as_ref().and_then(|r| r.end_line(cx)), |this, line| this.child(div().relative().child(line)))
                    .when(empty, |this| {
                        this.child(div().px_2().child(Label::new("No channels").size(12.).color(t.color_text_muted)))
                    }),
            );

        let delete = self.delete.clone().map(|(id, name, category)| {
            let server = server.unwrap_or(ServerId::NONE);
            let (title, message): (SharedString, SharedString) = if category {
                (format!("Delete category \"{name}\"?").into(), "Its channels stay and move up to the group above.".into())
            } else {
                (
                    format!("Delete #{name}?").into(),
                    "Every message and attachment in it will be permanently deleted. Cannot be undone.".into(),
                )
            };
            ConfirmModal::new("delete-channel", true, title, "Delete")
                .message(message)
                .tone(ConfirmTone::Danger)
                .on_confirm(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    call_or_toast(
                        cx,
                        "Couldn't delete the channel",
                        api::channels::delete_channel(api::channels::DeleteChannelArgs {
                            server_id: server.wire(),
                            channel_id: id.to_string(),
                        }),
                    );
                    this.delete = None;
                    cx.notify();
                }))
                .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.delete = None;
                    cx.notify();
                }))
        });

        sidebar_frame("server-sidebar", self.resize.width(), self.panel.read(cx).list_padding(), &t)
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<crate::sidebar::SidebarResize>, window, cx| {
                this.resize.drag_to(e, window, cx);
            }))
            .child(header)
            .child(list)
            .child(resize_handle("server-sidebar-resize", &t))
            .children(context_menu)
            .children(delete)
            .children(reorder.and_then(|r| r.overlay(window, cx)))
    }
}

/// The hover gear of a channel row a member may manage (opens its channel settings).
fn settings_gear(row_group: &SharedString, channel: ChannelId, t: &Ink) -> impl IntoElement + use<> {
    div()
        .id("channel-gear")
        .group("channel-gear")
        .ml_auto()
        .flex_none()
        .rounded(t.radius_sm)
        .p_0p5()
        .invisible()
        .group_hover(row_group.clone(), |s| s.visible())
        .tooltip(title_tooltip("Channel settings"))
        .on_click(move |_: &ClickEvent, _, cx| {
            cx.stop_propagation();
            let overlays = Model::global(cx).overlays.clone();
            overlays.update(cx, |o, cx| o.open_channel_settings(channel.clone(), cx));
        })
        .child(Icon::new(IconName::SettingsGear).size(px(14.)).color(t.color_text_muted).group_hover("channel-gear", t.color_text_primary))
}

/// The server menu under the header (`ServerActionsDropdown`): Server Settings, Invites (with
/// CREATE_INVITE or MANAGE_INVITES), Leave Server.
fn server_menu(server: ServerId, can_invite: bool, header: BoundsCell, cx: &Context<ChannelSidebar>) -> impl IntoElement + use<> {
    let t = crate::ink::chrome(cx);
    let item = |id: &'static str, icon: IconName, label: &'static str, danger: bool| {
        let (fg, fg_hover, bg_hover, icon_color) = if danger {
            (t.color_text_muted, t.color_error, t.color_error.opacity(0.1), t.color_text_muted)
        } else {
            (t.color_text_secondary, t.color_text_primary, t.color_surface_hover, t.color_text_muted)
        };
        div()
            .id(id)
            .group(id)
            .relative()
            .children(crate::probes::probe(id))
            .flex()
            .w_full()
            .items_center()
            .gap_2p5()
            .rounded(t.radius_sm)
            .px_2p5()
            .py(px(9.))
            .text_color(fg)
            .hover(move |s| s.bg(bg_hover).text_color(fg_hover))
            .cursor_pointer()
            .child(Icon::new(icon).size(px(16.)).color(icon_color).group_hover(id, if danger { t.color_error } else { t.color_text_secondary }))
            .child(Label::new(label).size(13.))
    };
    let close = |this: &mut ChannelSidebar, cx: &mut Context<ChannelSidebar>| {
        this.server_menu = false;
        cx.notify();
    };
    decibell_ui::layer(
        2,
        div()
            .id("server-menu")
            .occlude()
            // A press outside closes it; one on the header is the button's (it toggles).
            .on_mouse_down_out(cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                if !header.get().contains(&e.position) && this.server_menu {
                    this.server_menu = false;
                    cx.notify();
                }
            }))
            .absolute()
            .top_full()
            .left_2()
            .right_2()
            .mt_1p5()
            .rounded(t.radius_md)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_light)
            .p(px(5.))
            .shadow(cx.theme().chrome.shadow_float.clone())
            .child(item("server-menu-settings", IconName::ShellServerSettings, "Server Settings", false).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    close(this, cx);
                    let overlays = Model::global(cx).overlays.clone();
                    overlays.update(cx, |o, cx| o.open_modal(modal::SERVER_SETTINGS, cx));
                },
            )))
            .when(can_invite, |this| {
                this.child(item("server-menu-invites", IconName::SettingsLink, "Invites", false).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        close(this, cx);
                        let overlays = Model::global(cx).overlays.clone();
                        overlays.update(cx, |o, cx| o.open_modal(modal::INVITE_MANAGE, cx));
                    },
                )))
            })
            .child(div().mx_1p5().my_1().h(px(1.)).bg(t.color_border_divider))
            .child(item("server-menu-leave", IconName::SettingsLogout, "Leave Server", true).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    close(this, cx);
                    ShellRequests::leave_server(server, cx);
                },
            ))),
    )
}

/// The channel list's context menu (MANAGE_CHANNELS): settings and delete on a row, the create
/// actions on the empty area, then what feature crates registered.
fn channel_context_menu(
    at: Point<Pixels>,
    target: ChannelMenuTarget,
    channels: &[Channel],
    cx: &mut Context<ChannelSidebar>,
) -> impl IntoElement + use<> {
    // `w-[190px] rounded-md border border-border bg-bg-light p-[5px] shadow-float` with
    // `ContextMenuItem` rows, in the sidebar's DOM: the chrome's palette.
    let mut menu = ContextMenu::new("channel-menu", at)
        .width(px(190.))
        .sidebar()
        .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| this.close_menus(cx)));
    match &target {
        ChannelMenuTarget::Empty { server } => {
            let server = *server;
            for (label, kind) in [
                ("Create Text Channel", ChannelKind::Text),
                ("Create Voice Channel", ChannelKind::Voice),
                ("Create Category", ChannelKind::Category),
            ] {
                menu = menu.entry(MenuItem::new(label).on_click(move |_, _, cx| create_channel(server, kind, None, false, cx)));
            }
        }
        ChannelMenuTarget::Channel { channel, .. } | ChannelMenuTarget::Category { channel, .. } => {
            let category = matches!(target, ChannelMenuTarget::Category { .. });
            let name = channels.iter().find(|c| c.id == *channel).map(|c| c.name.clone()).unwrap_or_default();
            let id = channel.clone();
            menu = menu
                .entry(MenuItem::new(if category { "Category Settings" } else { "Channel Settings" }).on_click({
                    let id = id.clone();
                    move |_, _, cx| {
                        let overlays = Model::global(cx).overlays.clone();
                        overlays.update(cx, |o, cx| o.open_channel_settings(id.clone(), cx));
                    }
                }))
                .entry(
                    MenuItem::new(if category { "Delete Category" } else { "Delete Channel" }).danger().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.delete = Some((id.clone(), name.clone(), category));
                            cx.notify();
                        },
                    )),
                );
        }
    }
    let extensions = channel_menu_extensions(cx);
    let mut extra: Vec<MenuEntry> = Vec::new();
    for extension in extensions {
        extra.extend(extension(&target, cx));
    }
    decibell_ui::chrome_scope(menu.entries(extra))
}
