//! The composer card's small pieces, each copied from its TSX: the 34 px buttons, the reply bar
//! (`ReplyComposerBar`), the attachment chips (`PendingAttachmentsRow`), the slowmode pill
//! (`SlowmodeIndicator`), the send button, the drop overlay and the drop pulse.

use std::sync::Arc;
use std::time::Duration;

use decibell_attachments::AttachmentKind;
use decibell_common::{UserAvatar, display_name, person_name_color, user_name};
use decibell_model::chat::Message;
use decibell_model::chat::composer::{PendingAttachment, PendingStatus};
use decibell_model::ServerId;
use decibell_ui::motion::CubicBezier;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BoxShadow, ClickEvent, Div, Hsla, ObjectFit, SharedString, Stateful, Window, div, img,
    point, prelude::*, px,
};

/// `h-[34px] w-[34px]` icon buttons beside the input.
pub const BUTTON: f32 = 34.;

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A 34 px square icon button: `flex items-center justify-center rounded-md transition-colors`.
/// `active`: `bg-surface-hover text-text-secondary` (its menu or picker is open); otherwise
/// `text-text-muted hover:text-text-secondary`.
pub fn square_button(id: &'static str, icon: IconName, icon_px: f32, active: bool, rounded_sm: bool, title: Option<&'static str>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let radius = if rounded_sm { theme.radius_sm } else { theme.radius_md };
    let (muted, secondary) = (theme.color_text_muted, theme.color_text_secondary);
    let group = SharedString::new_static(id);
    let button = div()
        .id(id)
        .group(group.clone())
        .size(px(BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius)
        .cursor_pointer()
        .when(active, |this| this.bg(theme.color_surface_hover))
        .child(
            Icon::new(icon)
                .size(px(icon_px))
                .color(if active { secondary } else { muted })
                .group_hover(group, secondary),
        );
    match title {
        Some(title) => button.tooltip(title_tooltip(title)),
        None => button,
    }
}

/// `attachmentPreviewLabel`: "Image", "2 Videos", "3 Attachments".
pub fn kinds_label(kinds: &[AttachmentKind]) -> String {
    let label = |kind: AttachmentKind| match kind {
        AttachmentKind::Image => "Image",
        AttachmentKind::Video => "Video",
        AttachmentKind::Document => "File",
        AttachmentKind::Audio => "Audio",
    };
    if kinds.len() == 1 {
        return label(kinds[0]).to_string();
    }
    let first = kinds[0];
    if kinds.iter().all(|k| *k == first) {
        format!("{} {}s", kinds.len(), label(first))
    } else {
        format!("{} Attachments", kinds.len())
    }
}

fn kinds_glyph(kinds: &[AttachmentKind]) -> IconName {
    match kinds.iter().all(|k| *k == kinds[0]).then(|| kinds[0]) {
        Some(AttachmentKind::Image) => IconName::ChatKindImage,
        Some(AttachmentKind::Video) => IconName::ChatKindVideo,
        Some(AttachmentKind::Audio) => IconName::ChatKindAudio,
        Some(AttachmentKind::Document) => IconName::ChatKindDocument,
        None => IconName::ChatKindMixed,
    }
}

/// `ReplyComposerBar`: "Replying to <avatar> Name  snippet…  ×", ruled off from the input
/// (`flex min-w-0 items-center gap-1.5 border-b border-border-divider pb-2 text-reply
/// text-text-muted`).
pub fn reply_bar(server: Option<ServerId>, parent: &Arc<Message>, on_cancel: OnClick, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let size = theme.scaled.text_reply;
    let line = theme.scaled.text_reply_line_height;
    let muted = theme.color_text_muted;
    let uid = parent.sender_uid;
    let shown = display_name(server, uid, &parent.sender, cx);
    let account = user_name(uid, &parent.sender, cx);
    let name_color = person_name_color(uid, &account, cx);
    let kinds: Vec<AttachmentKind> = parent.attachments.iter().map(|a| a.kind).collect();
    let has_text = !parent.content.trim().is_empty();
    // MessageText's preview form: one line, markup gone (as the list's reply line draws it).
    let snippet = if has_text {
        let document = decibell_richtext::plain::preview(&parent.content);
        Some(decibell_richtext::plain::as_displayed(&document, &parent.content).replace('\n', " "))
    } else if !kinds.is_empty() {
        Some(kinds_label(&kinds))
    } else {
        None
    };
    let cancel_group = SharedString::new_static("reply-cancel");
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap_1p5()
        .pb_2()
        .border_b_1()
        .border_color(theme.color_border_divider)
        .child(Icon::new(IconName::ChatReply).size(px(14.)).color(theme.color_accent))
        .child(div().flex_none().child(Label::new("Replying to").size(size).line_height(line).color(muted).nowrap()))
        .child(div().flex_none().child(UserAvatar::new(uid, account.clone(), 16.)))
        .child(
            div()
                .flex_none()
                .max_w(gpui::relative(0.5))
                .min_w_0()
                .child(Label::new(shown).font(FontRole::Channel).size(size).line_height(line).weight(theme.palette.t_fw_emphasis as f32).color(name_color).truncate()),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .children(snippet.map(|s| div().min_w_0().child(Label::new(s).size(size).line_height(line).color(muted).truncate())))
                .when(!kinds.is_empty(), |this| this.child(Icon::new(kinds_glyph(&kinds)).size(px(14.)).color(muted))),
        )
        .child(
            div()
                .id("reply-cancel")
                .group(cancel_group.clone())
                .flex_none()
                .size_6()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .cursor_pointer()
                .hover(|s| s.bg(theme.color_surface_hover))
                .tooltip(title_tooltip("Cancel reply (Esc)"))
                .on_click(on_cancel)
                .child(Icon::new(IconName::ComposerX).size(px(14.)).color(muted).group_hover(cancel_group, theme.color_text_primary)),
        )
        .into_any_element()
}

/// `PendingAttachmentsRow`: the queued / uploading files of this message box (`flex flex-wrap
/// gap-2 border-t border-border-divider px-4 py-2`).
pub fn chips<'a>(items: impl Iterator<Item = &'a PendingAttachment>, on_remove: impl Fn(SharedString, &mut Window, &mut App) + Clone + 'static, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let mut row = div().flex().flex_wrap().gap_2().border_t_1().border_color(theme.color_border_divider).px_4().py_2();
    let mut any = false;
    for p in items {
        any = true;
        let pct = if p.total_bytes > 0 { ((p.transferred_bytes as f64 / p.total_bytes as f64) * 100.).round().min(100.) as u32 } else { 0 };
        let status: SharedString = match p.status {
            PendingStatus::Failed => p.error.clone().unwrap_or_else(|| "Failed".into()),
            PendingStatus::Ready => "Ready".into(),
            PendingStatus::Queued => format!("{pct}% · queued").into(),
            PendingStatus::Uploading => format!("{pct}% · uploading").into(),
        };
        let thumb = match (&p.preview, p.kind) {
            (Some(preview), AttachmentKind::Image) => img(preview.clone()).size_12().rounded(theme.radius_sm).object_fit(ObjectFit::Cover).into_any_element(),
            _ => div()
                .size_12()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .bg(theme.color_bg_secondary)
                .child(Icon::new(IconName::ChatKindDocument).size(px(20.)).color(theme.color_text_muted))
                .into_any_element(),
        };
        let id = p.id.clone();
        let remove = on_remove.clone();
        let remove_group: SharedString = format!("chip-remove-{}", p.id).into();
        row = row.child(
            div()
                .relative()
                .flex()
                .items_center()
                .gap_2()
                .rounded(theme.radius_md)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_light)
                .p_2()
                .child(thumb)
                .child(
                    div()
                        .min_w_0()
                        .child(div().max_w(px(160.)).child(Label::new(p.filename.clone()).font(FontRole::Channel).size(11.).color(theme.color_text_primary).truncate()))
                        .child(Label::new(status).size(10.).color(theme.color_text_muted).nowrap()),
                )
                .child(
                    div()
                        .id(gpui::ElementId::Name(remove_group.clone()))
                        .group(remove_group.clone())
                        .absolute()
                        .right(px(-4.))
                        .top(px(-4.))
                        .size_5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.color_bg_darkest)
                        .cursor_pointer()
                        // `hover:bg-error hover:text-white`
                        .hover(|s| s.bg(theme.color_error))
                        .tooltip(title_tooltip("Remove"))
                        .on_click(move |_, window, cx| remove(id.clone(), window, cx))
                        // "×" in text-secondary, white while hovered (a label's colour is fixed when
                        // drawn: two labels, the hover one shown by the group's hover).
                        .child(
                            div()
                                .relative()
                                .child(div().group_hover(remove_group.clone(), |s| s.opacity(0.)).child(Label::new("×").color(theme.color_text_secondary).nowrap()))
                                .child(div().absolute().top_0().left_0().opacity(0.).group_hover(remove_group, |s| s.opacity(1.)).child(Label::new("×").color(gpui::white()).nowrap())),
                        ),
                ),
        );
    }
    any.then(|| row.into_any_element())
}

/// SlowmodeIndicator's `short`: "30s", "1m 30s", "2h".
pub fn slowmode_short(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let parts: Vec<String> = [(h, "h"), (m, "m"), (s, "s")].iter().filter(|(n, _)| *n > 0).map(|(n, unit)| format!("{n}{unit}")).collect();
    if parts.is_empty() { "0s".into() } else { parts.join(" ") }
}

/// `spoken`: "24 seconds", "1 minute 30 seconds".
pub fn slowmode_spoken(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let part = |n: u32, unit: &str| if n > 0 { format!("{n} {unit}{}", if n == 1 { "" } else { "s" }) } else { String::new() };
    let parts: Vec<String> = [part(h, "hour"), part(m, "minute"), part(s, "second")].into_iter().filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { "0 seconds".into() } else { parts.join(" ") }
}

/// `clock`: "0:24", "4:59", "1:05:00".
pub fn slowmode_clock(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `barNudge` (0.36 s `ease`): translateX 0 → −6 → 5 → −3 → 2 → 0 at 0/20/40/60/80/100 %.
fn nudge_x(t: f32) -> f32 {
    let keys = [(0., 0.), (0.2, -6.), (0.4, 5.), (0.6, -3.), (0.8, 2.), (1., 0.)];
    for pair in keys.windows(2) {
        let ((t0, x0), (t1, x1)) = (pair[0], pair[1]);
        if t <= t1 {
            // CSS applies the timing function per keyframe interval.
            let local = ((t - t0) / (t1 - t0)).clamp(0., 1.);
            let eased = CubicBezier::new(0.25, 0.1, 0.25, 1.).y(local as f64) as f32;
            return x0 + (x1 - x0) * eased;
        }
    }
    0.
}

/// `SlowmodeIndicator`: the interval while you may post, a countdown after each message;
/// `nudge` > 0 shakes it (a send held back), keyed so each new nudge shakes once.
pub fn slowmode_pill(interval: u32, remaining: u32, nudge: u32, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let waiting = remaining > 0;
    let label = if waiting {
        format!("Slowmode: you can send again in {}.", slowmode_spoken(remaining))
    } else {
        format!("Slowmode: one message every {}.", slowmode_spoken(interval))
    };
    let color = if waiting { theme.color_accent_bright } else { theme.color_text_muted };
    let pill = div()
        .id("slowmode")
        .relative()
        .h(px(BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .rounded(theme.radius_md)
        .when(waiting, |this| this.bg(theme.color_accent_soft))
        .tooltip(title_tooltip(label))
        .child(Icon::new(IconName::SettingsHourglass).size(px(14.)).color(color))
        .child(Label::new(if waiting { slowmode_clock(remaining) } else { slowmode_short(interval) }).size(12.).weight(600.).color(color).nowrap());
    if nudge == 0 {
        return pill.into_any_element();
    }
    pill.with_animation(("slowmode-nudge", nudge as usize), Animation::new(Duration::from_millis(360)), |this, t| this.left(px(nudge_x(t))))
        .into_any_element()
}

/// The send button (`SendButton` / `DmSendButton`): 34 px, the paper plane; disabled without
/// anything to send (`disabled:bg-surface-active disabled:text-text-muted`), "held" during a
/// slowmode wait (looks off but takes the click, which shakes the pill).
pub fn send_button(enabled: bool, held: bool, dm: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let off = !enabled || held;
    let (bg, fg) = if off { (theme.color_surface_active, theme.color_text_muted) } else { (theme.color_accent, theme.color_on_accent) };
    let hover = theme.color_accent_hover;
    let button = div()
        .id("send")
        .size(px(BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(if dm { theme.radius_sm } else { theme.radius_md })
        .bg(bg)
        .when(!off, |this| this.cursor_pointer().hover(move |s| s.bg(hover)))
        .when(off && !dm, |this| this.cursor(gpui::CursorStyle::OperationNotAllowed))
        .when(dm && !enabled, |this| this.cursor(gpui::CursorStyle::OperationNotAllowed))
        .when(dm && enabled, |this| this.cursor_pointer())
        .child(Icon::new(IconName::ComposerSend).size(px(16.)).color(fg));
    if dm {
        button
    } else {
        button.tooltip(title_tooltip(if held { "Slowmode is on" } else { "Send" }))
    }
}

/// The drop overlay over the card while a file is dragged over it (`bg-gradient-to-b
/// from-accent-soft/80 to-accent-soft/40 backdrop-blur-[3px]`: GPUI has no backdrop blur).
pub fn drop_overlay(channel_name: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let soft = theme.color_accent_soft;
    let top = Hsla { a: soft.a * 0.8, ..soft };
    let bottom = Hsla { a: soft.a * 0.4, ..soft };
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(theme.radius_lg)
        .bg(gpui::linear_gradient(180., gpui::linear_color_stop(top, 0.), gpui::linear_color_stop(bottom, 1.)))
        .child(Icon::new(IconName::ComposerUpload).size(px(20.)).color(theme.color_accent_bright))
        .child(Label::new(format!("Drop to upload to #{channel_name}")).size(13.).weight(600.).color(theme.color_accent_bright).nowrap())
        .into_any_element()
}

/// `.drop-pulse::after`: a 1.5 px accent ring (60 %) and a 22 px / 4 px bloom (22 %) on a layer
/// whose opacity breathes 0.4 → 1 → 0.4 over 1.6 s (`breathe`, ease-in-out, while a drag lasts).
/// The ring and bloom are the component rule's own literals (css-component-rules.md).
pub fn drop_pulse(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let accent = theme.color_accent;
    let ring = BoxShadow { color: Hsla { a: accent.a * 0.6, ..accent }, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.5), inset: false };
    // A CSS blur radius is twice GPUI's (CONVENTIONS "Theme").
    let bloom = BoxShadow { color: Hsla { a: accent.a * 0.22, ..accent }, offset: point(px(0.), px(0.)), blur_radius: px(11.), spread_radius: px(4.), inset: false };
    div()
        .absolute()
        .inset_0()
        .rounded(theme.radius_lg)
        .shadow(vec![bloom, ring])
        .with_animation(
            "drop-pulse",
            Animation::new(Duration::from_millis(1600)).repeat().with_easing(decibell_ui::motion::EASE_IN_OUT.easing()),
            |this, t| {
                // 0 → 0.5 → 1 of the cycle: 0.4 → 1 → 0.4.
                let opacity = if t < 0.5 { 0.4 + 1.2 * t } else { 1. - 1.2 * (t - 0.5) };
                this.opacity(opacity)
            },
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SlowmodeIndicator's three formats.
    #[test]
    fn slowmode_labels_read_as_the_renderers() {
        assert_eq!(slowmode_short(30), "30s");
        assert_eq!(slowmode_short(90), "1m 30s");
        assert_eq!(slowmode_short(7200), "2h");
        assert_eq!(slowmode_short(0), "0s");
        assert_eq!(slowmode_spoken(1), "1 second");
        assert_eq!(slowmode_spoken(90), "1 minute 30 seconds");
        assert_eq!(slowmode_spoken(3601), "1 hour 1 second");
        assert_eq!(slowmode_clock(24), "0:24");
        assert_eq!(slowmode_clock(299), "4:59");
        assert_eq!(slowmode_clock(3900), "1:05:00");
    }

    /// The reply bar's label for a parent with attachments only.
    #[test]
    fn attachment_only_parents_are_named_by_kind() {
        assert_eq!(kinds_label(&[AttachmentKind::Image]), "Image");
        assert_eq!(kinds_label(&[AttachmentKind::Video, AttachmentKind::Video]), "2 Videos");
        assert_eq!(kinds_label(&[AttachmentKind::Document, AttachmentKind::Image, AttachmentKind::Audio]), "3 Attachments");
        assert_eq!(kinds_glyph(&[AttachmentKind::Audio, AttachmentKind::Image]), IconName::ChatKindMixed);
    }

    /// barNudge's keyframes: at rest at both ends, −6 px a fifth of the way in.
    #[test]
    fn the_nudge_shakes_and_settles() {
        assert_eq!(nudge_x(0.), 0.);
        assert!((nudge_x(0.2) + 6.).abs() < 1e-4);
        assert!((nudge_x(0.4) - 5.).abs() < 1e-4);
        assert_eq!(nudge_x(1.), 0.);
    }
}
