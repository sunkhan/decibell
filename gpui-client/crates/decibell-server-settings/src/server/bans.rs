//! Bans (`tabs/BansTab.tsx`): the ban list (only BAN_MEMBERS holders receive it) with the
//! moderator, the date, the expiry and the reason, and Unban. The list after an unban arrives as
//! the pushed `ban_list_received`. Windowed: only the rows near the pane's viewport are built.

use std::collections::HashSet;

use decibell_common::{UserAvatar, Watch, user_name};
use decibell_model::regional::{self, Regional};
use decibell_model::stores::community::Ban;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, Uid};
use decibell_ui::{ActiveTheme as _, Icon, IconName, Label};
use gpui::{App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use super::{SettingsTab, TabContext};
use crate::requests::{self, Request};
use crate::rows::PaneRows;
use crate::ui::{Focuses, focusable};

/// A row (`py-2.5`, a 36 px avatar) and the list's `gap-1`.
const ROW: f32 = 56. + 4.;

pub struct BansTab {
    ctx: TabContext,
    error: Option<SharedString>,
    pending: HashSet<Uid>,
    /// Each row's Unban: a tab stop (a `<button>` there).
    focus: Focuses,
    _watch: Watch,
}

impl SettingsTab for BansTab {
    fn dirty(&self, _: &App) -> bool {
        false
    }
}

impl BansTab {
    pub fn new(ctx: TabContext, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.settings, cx).people(cx, |_, _| true).store(&model.auth, cx);
        if let Some(community) = model.community(ctx.server, cx) {
            watch.store(&community.moderation, cx).store(&community.roles, cx).store(&community.meta, cx);
        }
        BansTab { ctx, error: None, pending: HashSet::new(), focus: Focuses::default(), _watch: watch }
    }

    pub(crate) fn unban(&mut self, uid: Uid, username: SharedString, cx: &mut Context<Self>) {
        self.pending.insert(uid);
        self.error = None;
        cx.notify();
        let weak = cx.weak_entity();
        requests::send(Request::UnbanMember { server: self.ctx.server, username: username.to_string(), uid }, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.pending.remove(&uid);
                if let Err(e) = result {
                    this.error = Some(e.js_string());
                }
                cx.notify();
            });
        });
    }
}

/// The detail line: `by alice · 11/7/2026 · until Nov 9, 2026, 2:32 PM · spam`.
pub fn ban_details(b: &Ban, cx: &App) -> String {
    let regional = Regional::get();
    let pref = Model::global(cx).settings.read(cx).time_format();
    let mut line = String::new();
    if !b.banned_by.is_empty() {
        line.push_str("by ");
        line.push_str(&user_name(b.banned_by_uid, &b.banned_by, cx));
    }
    if b.banned_at != 0 {
        line.push_str(" · ");
        if let Some(t) = regional::local(b.banned_at) {
            line.push_str(&regional.short_date(t.date));
        }
    }
    if b.expires_at != 0 {
        line.push_str(" · until ");
        line.push_str(&regional.date_time(b.expires_at, pref));
    } else {
        line.push_str(" · permanent");
    }
    if !b.reason.is_empty() {
        line.push_str(" · ");
        line.push_str(&b.reason);
    }
    line
}

impl Render for BansTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let server = self.ctx.server;
        let (bans, truncated) = model
            .community(server, cx)
            .map(|c| {
                let m = c.moderation.read(cx);
                (m.bans.clone(), m.bans_truncated)
            })
            .unwrap_or_default();
        let can_ban = model.has_permission(server, perm::BAN_MEMBERS, cx);
        let theme = cx.theme();
        let mut out = div().flex().flex_col();
        if let Some(error) = &self.error {
            out = out.child(div().mb_3().child(Label::new(error.clone()).size(12.).color(theme.color_error)));
        }
        if truncated {
            out = out.child(
                div().mb_3().child(
                    Label::new(format!("Showing the most recent {} bans.", Regional::get().group(bans.len() as u64)))
                        .size(12.)
                        .color(theme.color_text_muted),
                ),
            );
        }
        if bans.is_empty() {
            return out.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2p5()
                    .py_8()
                    .child(
                        div()
                            .flex()
                            .size_10()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme.color_text_muted.opacity(0.1))
                            .child(Icon::new(IconName::ServerTabBans).size(px(18.)).color(theme.color_text_muted)),
                    )
                    .child(Label::new("No bans.").size(13.).color(theme.color_text_muted)),
            );
        }
        let pending = self.pending.clone();
        let weak = cx.weak_entity();
        let focus = self.focus.clone();
        let count = bans.len();
        out.child(PaneRows::uniform(&self.ctx.scroll, count, px(ROW), move |range, _, cx| {
            let theme = cx.theme();
            range
                .map(|i| {
                    let b = &bans[i];
                    let name = user_name(b.uid, &b.username, cx);
                    let details = ban_details(b, cx);
                    let busy = pending.contains(&b.uid);
                    let (uid, weak, unban_name) = (b.uid, weak.clone(), name.clone());
                    let (muted, success, row_hover) = (theme.color_text_muted, theme.color_success, theme.color_surface_hover);
                    div()
                        .id(("ban", b.uid as u64))
                        .flex()
                        .h(px(56.))
                        .items_center()
                        .gap_3()
                        .rounded(theme.radius_md)
                        .px_3()
                        .hover(move |s| s.bg(row_hover))
                        .child(div().opacity(0.5).child(UserAvatar::new(b.uid, name.clone(), 36.)))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(Label::new(name).size(13.).weight(500.).color(theme.color_text_secondary).truncate())
                                .child(Label::new(details).size(11.).line_height(16.5).color(theme.color_text_muted).truncate()),
                        )
                        .when(can_ban, |this| {
                            let bg = theme.color_bg_light;
                            this.child(
                                focusable(div().id(("unban", b.uid as u64)), &focus.get(format!("unban:{uid}"), cx), cx)
                                    .rounded(theme.radius_sm)
                                    .bg(bg)
                                    .px_2p5()
                                    .py_1p5()
                                    .text_color(muted)
                                    .child(Label::new("Unban").size(11.).weight(500.))
                                    .when(busy, |this| this.opacity(0.5))
                                    .when(!busy, |this| {
                                        this.cursor_pointer().hover(move |s| s.bg(success.opacity(0.15)).text_color(success)).on_click(
                                            move |_, _, cx| {
                                                let name = unban_name.clone();
                                                let _ = weak.update(cx, |this, cx| this.unban(uid, name, cx));
                                            },
                                        )
                                    }),
                            )
                        })
                        .child(
                            div().rounded(theme.radius_sm).bg(theme.color_error.opacity(0.15)).px_1p5().py_0p5().child(
                                Label::new(if b.expires_at != 0 { "TEMP BAN" } else { "BANNED" })
                                    .size(10.)
                                    .weight(600.)
                                    .tracking(0.04)
                                    .color(theme.color_error),
                            ),
                        )
                        .into_any_element()
                })
                .collect()
        }))
    }
}
