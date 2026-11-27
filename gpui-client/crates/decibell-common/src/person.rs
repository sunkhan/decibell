//! A person on screen: their avatar (picture or letter) with the presence interest it implies,
//! their name as shown in a community, their status dot and their status message
//! (`components/UserAvatar.tsx`, `hooks/useDisplayName.ts`, `stores/presenceStore.ts`'
//! `usePresence`, `components/StatusMessageText.tsx`).
//!
//! These read the model in `render`; the view that draws them keeps itself current with
//! [`crate::Watch::people`] (names, pictures, statuses and messages of the uids it shows).

use decibell_model::live::presence::DisplayStatus;
use decibell_model::live::presence::avatars::AvatarStatus;
use decibell_model::live::presence::statuses::StatusMessage;
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_ui::identity::{name_color, person_slot};
use decibell_ui::{ActiveTheme as _, FontRole, Label, LetterAvatar, Presence, StatusDot};
use gpui::{
    App, Context, Hsla, ImageSource, IntoElement, ObjectFit, RenderOnce, SharedString, Window, div, img, prelude::*,
    px, rgb,
};

/// What people see when nobody named a uid (`"Unknown user"` in the renderer).
pub const UNKNOWN_USER: &str = "Unknown user";

/// The name shown for `uid` (`useUserName`): the current name the servers sent, else `fallback`
/// (the name the caller's record carries), else "Unknown user".
pub fn user_name(uid: Uid, fallback: &SharedString, cx: &App) -> SharedString {
    let name = Model::global(cx).name_of(uid, fallback, cx);
    if name.is_empty() { SharedString::new_static(UNKNOWN_USER) } else { name }
}

/// The name shown for `uid` in a community (`useDisplayName`): their server nickname, else
/// their current name, else `fallback`. `server` none (DMs) skips the nickname. A member the
/// roster hasn't loaded shows their name until a lookup brings the row: call
/// [`want_member`] where such a row is built.
pub fn display_name(server: Option<ServerId>, uid: Uid, fallback: &SharedString, cx: &App) -> SharedString {
    let name = Model::global(cx).display_name(server, uid, fallback, cx);
    if name.is_empty() { SharedString::new_static(UNKNOWN_USER) } else { name }
}

/// Look `uid` up in `server`'s roster when it isn't loaded (`useMemberInterest`): a message author,
/// a voice participant, a profile. Deferred to after the frame, so it can be called while rows
/// are built.
pub fn want_member(server: ServerId, uid: Uid, cx: &mut App) {
    cx.defer(move |cx| {
        if let Some(community) = Model::global(cx).community(server, cx) {
            community.roster.update(cx, |roster, cx| roster.want(uid, cx));
        }
    });
}

/// Keeps a view that shows **one** member's name in a community current, as `useDisplayName` does
/// for each component that calls it: [`NicknameWatch::follow`] looks the member up when the roster
/// hasn't loaded them and notifies the view whenever their nickname there arrives, changes or goes
/// (renames come through [`crate::Watch::people`]). Hold it in the view and call `follow` again when
/// the person or the community changes (a no-op while they are the same). A view that shows many
/// people checks its rows in one roster observer instead (the message list, the audit log).
#[derive(Default)]
pub struct NicknameWatch {
    key: Option<(ServerId, Uid)>,
    _roster: Option<gpui::Subscription>,
}

impl NicknameWatch {
    pub fn follow<V: 'static>(&mut self, server: Option<ServerId>, uid: Uid, cx: &mut Context<V>) {
        let key = server.filter(|_| uid != 0).map(|server| (server, uid));
        if key == self.key {
            return;
        }
        self.key = key;
        self._roster = None;
        let Some((server, uid)) = key else { return };
        want_member(server, uid, cx);
        let Some(community) = Model::global(cx).community(server, cx) else { return };
        let mut nickname = community.roster.read(cx).nickname(uid).cloned();
        // The roster notifies for every delta: only a change of this member's nickname redraws.
        self._roster = Some(cx.observe(&community.roster, move |_, roster, cx| {
            let now = roster.read(cx).nickname(uid).cloned();
            if now != nickname {
                nickname = now;
                cx.notify();
            }
        }));
    }
}

/// A person's name colour (`userColor`: the uid's slot of the `--color-name-*` ramp).
pub fn person_name_color(uid: Uid, name: &str, cx: &App) -> Hsla {
    name_color(cx.theme(), person_slot(uid, name))
}

/// A role's colour (`roleColor`, Server Settings): `0xRRGGBB`, `None` for "no colour" (the caller
/// draws its muted token).
pub fn role_color(color: u32) -> Option<Hsla> {
    // A literal by nature: the role's own colour, chosen by its moderators.
    (color != 0).then(|| rgb(color).into())
}

/// `usePresence(uid)`: ourselves as others see us, anyone else from central, the community's
/// online flag standing in until central has answered.
pub fn presence_of(uid: Uid, community_online: bool, cx: &App) -> DisplayStatus {
    let model = Model::global(cx);
    let me = model.auth.read(cx).uid;
    model.live.presence.display(me, uid, community_online, cx)
}

/// The dot for a status (an invisible user is drawn offline, as the renderer's `StatusDot` does).
pub fn dot_presence(status: DisplayStatus) -> Presence {
    match status {
        DisplayStatus::Online => Presence::Online,
        DisplayStatus::Away => Presence::Away,
        DisplayStatus::Dnd => Presence::Dnd,
        DisplayStatus::Offline | DisplayStatus::Invisible => Presence::Offline,
    }
}

/// `useStatusMessage(uid)`: ours always, others' only while they are visible.
pub fn status_message_of(uid: Uid, cx: &App) -> Option<StatusMessage> {
    let model = Model::global(cx);
    let me = model.auth.read(cx).uid;
    model.live.presence.message(me, uid, cx).cloned()
}

/// The avatar's status dot: on a disc of `ring` (the surface behind it), `size` px
/// (`avatar-dot`: 13 px × the list density), at the avatar's bottom-right corner (`-bottom-px
/// -right-px`).
#[derive(Clone, Copy)]
pub struct AvatarDot {
    pub status: DisplayStatus,
    pub ring: Hsla,
    pub size: f32,
    pub ring_width: f32,
}

impl AvatarDot {
    pub fn new(status: DisplayStatus, ring: Hsla, size: f32) -> Self {
        Self { status, ring, size, ring_width: 2.5 }
    }

    pub fn ring_width(mut self, width: f32) -> Self {
        self.ring_width = width;
        self
    }
}

/// Holds a uid in the presence watch set (and fetches its picture) while an avatar showing it is
/// drawn in consecutive frames; released when it stops being drawn.
struct Interest;

impl Interest {
    fn new(uid: Uid, cx: &mut Context<Self>) -> Self {
        cx.defer(move |cx| {
            actions::presence::retain([uid], cx);
            fetch_if_idle(uid, cx);
        });
        cx.on_release(move |_, cx| actions::presence::release([uid], cx)).detach();
        Interest
    }
}

fn fetch_if_idle(uid: Uid, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let avatars = model.live.presence.avatars.clone();
    if avatars.read(cx).get(uid).is_some_and(|e| e.status == AvatarStatus::Idle) {
        avatars.update(cx, |a, cx| a.fetch_if_needed(uid, cx));
    }
}

/// The self-fetching avatar (`UserAvatar`): the picture when the avatar store has it, the letter
/// avatar meanwhile (loading, missing, failed, unknown). While drawn it keeps the uid in the
/// presence watch set, so central sends the person's status, renames and picture changes, and it
/// asks for a picture whose version is known but not loaded. Optionally with the status dot.
///
/// ```ignore
/// UserAvatar::new(member.uid, &member.username, 34. * row_scale)
///     .dot(AvatarDot::new(presence_of(uid, member.is_online, cx), theme.color_bg_tertiary, 13. * row_scale))
/// ```
///
/// Its element id is `("user-avatar", uid)` under the nearest ancestor with an id: give list rows
/// their own ids. The view drawing it watches [`crate::Watch::people`].
#[derive(IntoElement)]
pub struct UserAvatar {
    uid: Uid,
    name: SharedString,
    size: f32,
    dot: Option<AvatarDot>,
    dimmed: bool,
    chrome: bool,
}

impl UserAvatar {
    /// `name`: the name the caller has for them (the letter until the users store knows the
    /// current one). `size` in px: multiply by the list density where the renderer's row
    /// publishes `--avatar-scale`.
    pub fn new(uid: Uid, name: impl Into<SharedString>, size: f32) -> Self {
        Self { uid, name: name.into(), size, dot: None, dimmed: false, chrome: false }
    }

    /// Inside the chrome (server bar, sidebars, user panel: `.chrome-scope`): the letter's colours
    /// come from the chrome palette, which differs from the base one in console-split.
    pub fn chrome(mut self) -> Self {
        self.chrome = true;
        self
    }

    pub fn dot(mut self, dot: AvatarDot) -> Self {
        self.dot = Some(dot);
        self
    }

    /// `opacity-[0.62]` on the avatar and its dot (offline members).
    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }
}

impl RenderOnce for UserAvatar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let uid = self.uid;
        let size = px(self.size);
        let face = match cx.try_global::<Model>().cloned() {
            Some(model) => {
                let shown = model.name_of(uid, &self.name, cx);
                if uid > 0 {
                    window.use_keyed_state(("user-avatar", uid as u64), cx, |_, cx| Interest::new(uid, cx));
                }
                let entry = model.live.presence.avatars.read(cx).get(uid);
                match entry.map(|e| (e.status, e.image.clone())) {
                    Some((AvatarStatus::Loaded, Some(image))) => img(ImageSource::Image(image))
                        .size(size)
                        .flex_none()
                        .object_fit(ObjectFit::Cover)
                        .rounded(cx.theme().avatar_radius(size))
                        .into_any_element(),
                    status => {
                        // A version announced since the last fetch: fetch it now that it shows.
                        if matches!(status, Some((AvatarStatus::Idle, _))) {
                            cx.defer(move |cx| fetch_if_idle(uid, cx));
                        }
                        letter(uid, shown, self.size, self.chrome, cx)
                    }
                }
            }
            None => letter(uid, self.name, self.size, self.chrome, cx),
        };
        let Some(dot) = self.dot else {
            return div().flex_none().when(self.dimmed, |this| this.opacity(0.62)).child(face);
        };
        div()
            .relative()
            .flex_none()
            .size(size)
            .when(self.dimmed, |this| this.opacity(0.62))
            .child(face)
            .child(
                div().absolute().bottom(px(-1.)).right(px(-1.)).child(
                    StatusDot::new(dot_presence(dot.status), dot.ring).size(dot.size).ring_width(dot.ring_width),
                ),
            )
    }
}

/// The letter avatar: the kit's `LetterAvatar`, or the same drawn from the chrome palette (the kit's
/// reads the base scope only).
fn letter(uid: Uid, name: SharedString, size: f32, chrome: bool, cx: &App) -> gpui::AnyElement {
    if !chrome {
        return LetterAvatar::new(uid, name, size).into_any_element();
    }
    let theme = cx.theme();
    let t = &theme.chrome;
    let base = [t.color_av_1, t.color_av_2, t.color_av_3, t.color_av_4, t.color_av_5, t.color_av_6, t.color_av_7, t.color_av_8]
        [person_slot(uid, &name).clamp(1, 8) - 1];
    // `color-mix(in srgb, c 72%, #000)`.
    let mut dark: gpui::Rgba = base.into();
    dark.r *= 0.72;
    dark.g *= 0.72;
    dark.b *= 0.72;
    let initial: SharedString =
        name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "?".into()).into();
    let font_size = (size * 0.42).floor().max(10.);
    div()
        .flex()
        .flex_none()
        .size(px(size))
        .items_center()
        .justify_center()
        .rounded(theme.avatar_radius(px(size)))
        .bg(gpui::linear_gradient(135., gpui::linear_color_stop(base, 0.), gpui::linear_color_stop(dark, 1.)))
        .child(Label::new(initial).size(font_size).line_height(font_size * 1.5).weight(600.).color(t.color_av_fg))
        .into_any_element()
}

/// A custom status message (`StatusMessageText`): its emoji, then its text, one line cut with
/// "…". The caller gives the line's font, size and colour (`font-meta text-micro text-text-muted`
/// in lists).
///
/// The renderer draws the emoji as Twemoji; here they come from the text engine's font fallback
/// until the rich-text engine's inline emoji are wired into labels (decibell-text).
#[derive(IntoElement)]
pub struct StatusMessageText {
    message: StatusMessage,
    font: FontRole,
    size: f32,
    line_height: Option<f32>,
    color: Option<Hsla>,
}

impl StatusMessageText {
    pub fn new(message: StatusMessage) -> Self {
        Self { message, font: FontRole::Meta, size: 11., line_height: None, color: None }
    }

    pub fn font(mut self, font: FontRole) -> Self {
        self.font = font;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn line_height(mut self, px: f32) -> Self {
        self.line_height = Some(px);
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

/// The text a status message reads as: `"<emoji> <text>"`, either part optional.
pub fn status_line(message: &StatusMessage) -> SharedString {
    match (message.emoji.is_empty(), message.text.is_empty()) {
        (false, false) => format!("{} {}", message.emoji, message.text).into(),
        (false, true) => message.emoji.clone(),
        _ => message.text.clone(),
    }
}

impl RenderOnce for StatusMessageText {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut label = Label::new(status_line(&self.message))
            .font(self.font)
            .size(self.size)
            .color(self.color.unwrap_or(cx.theme().color_text_muted))
            .truncate();
        if let Some(line_height) = self.line_height {
            label = label.line_height(line_height);
        }
        label
    }
}
