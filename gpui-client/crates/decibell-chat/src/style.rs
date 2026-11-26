//! Everything a row takes from the theme, resolved once per (theme, text size, window scale):
//! text styles, colours and the chrome's numbers. Rows are laid out and painted from this and
//! never read the theme themselves, so a scroll frame touches no theme lookups.
//!
//! The numbers are `MessageBubble.tsx`'s Tailwind classes, each named where it is used. Message
//! chrome does not follow the list density (`--ui-row-scale`): only text sizes move, with the
//! user's text size (`theme.scaled`).

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use decibell_text::{CodeStyle, Engine, Face, FontSpec, KeyHasher, LineHeight, MathColors, RichStyle, TextStyle};
use decibell_ui::{ActiveTheme as _, FontRole, Theme};
use gpui::{App, Hsla, Pixels, px};

/// `<body>`'s `letter-spacing: var(--t-ls-body)` is an em value computed against the browser's
/// 16 px and inherited as px by all text (as `decibell_ui::Label` does).
const BODY_FONT_SIZE: f32 = 16.;

/// Chromium's default selection colours over the app's ground (the renderer has no
/// `::selection` rule; measured in Electron 33, `decibell-text` README).
const SELECTION_BACKGROUND: u32 = 0x0538a4;
const SELECTION_TEXT: u32 = 0xffffff;

/// `style={{ paddingLeft: 12 }}` (ChatPanel passes 12: the avatar lines up with the composer card).
pub const ROW_PAD_LEFT: f32 = 12.;
/// `pr-2`.
pub const ROW_PAD_RIGHT: f32 = 8.;
/// `pt-2.5` / `pb-0.5` of a row that opens a group.
pub const ROW_PAD_TOP: f32 = 10.;
pub const ROW_PAD_BOTTOM: f32 = 2.;
/// `py-px` of a grouped row.
pub const GROUPED_PAD_Y: f32 = 1.;
/// `UserAvatar size={38}` and the grouped row's `w-[38px]` time gutter.
pub const AVATAR: f32 = 38.;
/// `gap-3` between the avatar and the content.
pub const GUTTER: f32 = 12.;
/// `gap-2` between the name and the time.
pub const HEADER_GAP: f32 = 8.;
/// `mt-0.5` of the body under the header.
pub const BODY_MARGIN_TOP: f32 = 2.;
/// DayDivider: `pt-5 pb-1 pl-3 pr-2 gap-3`.
pub const DIVIDER_PAD_TOP: f32 = 20.;
pub const DIVIDER_PAD_BOTTOM: f32 = 4.;
pub const DIVIDER_GAP: f32 = 12.;
/// ReplyHeader: `MINI_AVATAR_PX`, `mb-1`, `gap-1.5`, the name's `max-w-56`.
pub const REPLY_AVATAR: f32 = 16.;
pub const REPLY_MARGIN_BOTTOM: f32 = 4.;
pub const REPLY_GAP: f32 = 6.;
pub const REPLY_NAME_MAX: f32 = 224.;
/// The hover actions: `h-6 w-6` buttons, `gap-1`, `right-2`, `top-1` (`top-0` when grouped),
/// a `h-3.5 w-3.5` glyph.
pub const ACTION: f32 = 24.;
pub const ACTION_GAP: f32 = 4.;
pub const ACTION_RIGHT: f32 = 8.;
pub const ACTION_GLYPH: f32 = 14.;
/// The rail of a reply to me: `before:inset-y-1 before:left-0 before:w-0.5`.
pub const RAIL_INSET_Y: f32 = 4.;
pub const RAIL_WIDTH: f32 = 2.;

/// CodeBlock.tsx: `my-1`, the 1 px border, the header (`px-3 py-1` around the `h-6` copy
/// button, `border-b`), the `<pre>` (`px-3 py-2.5`, 12.5 px / 1.5), `max-h-60` while collapsed,
/// the expand bar (`border-t`, `px-3 py-1.5`), `min-w-48`.
pub const CODE_MARGIN_Y: f32 = 4.;
pub const CODE_BORDER: f32 = 1.;
pub const CODE_HEADER: f32 = 4. + 24. + 4. + 1.;
pub const CODE_PAD_X: f32 = 12.;
pub const CODE_PAD_Y: f32 = 10.;
pub const CODE_FONT: f32 = 12.5;
pub const CODE_LINE: f32 = 18.75;
pub const CODE_COLLAPSED_MAX: f32 = 240.;
pub const CODE_COLLAPSE_LINES: usize = 14;
pub const CODE_MIN_WIDTH: f32 = 192.;
pub const CODE_FOOT_PAD_Y: f32 = 6.;
/// A classic scrollbar's thickness inside a code block (`scrollbar-width: thin`, as the kit's
/// `ScrollArea` measured it).
pub const BAR: f32 = 10.;
/// MathTex.tsx's display formula: `my-1 py-1`.
pub const MATH_MARGIN_Y: f32 = 4.;
/// A notice card row (`DmChatPanel`'s refusal rows: `pl-4 pr-2 py-1.5` around an `ErrorCard`).
pub const CARD_ROW_PAD_LEFT: f32 = 16.;
pub const CARD_ROW_PAD_RIGHT: f32 = 8.;
pub const CARD_ROW_PAD_Y: f32 = 6.;
/// `ErrorCard`: `max-w-[520px] border px-4 py-3.5 gap-3`, the icon's `mt-px h-8 w-8` circle.
pub const CARD_MAX_WIDTH: f32 = 520.;
pub const CARD_BORDER: f32 = 1.;
pub const CARD_PAD_X: f32 = 16.;
pub const CARD_PAD_Y: f32 = 14.;
pub const CARD_GAP: f32 = 12.;
pub const CARD_ICON: f32 = 32.;
pub const CARD_ICON_MARGIN_TOP: f32 = 1.;
pub const MATH_PAD_Y: f32 = 4.;

/// See the module doc.
pub struct ChatStyle {
    /// Names the layout-relevant part (fonts, sizes, colours baked into paragraphs, scale).
    pub key: u64,
    pub scale: f32,
    pub rich: RichStyle,
    /// The same with every colour at half alpha: a pending bubble (`opacity-50` on the content).
    pub rich_pending: RichStyle,
    pub code: CodeStyle,
    pub math: MathColors,
    /// `font-channel text-sender font-emphasis` (coloured per sender at paint).
    pub name: TextStyle,
    /// `font-meta text-meta font-normal` (the header time, the day label at weight 500).
    pub time: TextStyle,
    /// The grouped row's hover time: the same with `leading-none`.
    pub hover_time: TextStyle,
    pub day: TextStyle,
    /// `text-reply` at its paired line height: the quoted line's snippet and name.
    pub reply: TextStyle,
    pub reply_name: TextStyle,
    /// The decrypt-error line: `text-body italic leading-body`.
    pub notice: TextStyle,
    /// `ErrorCard`'s paragraph (`text-[13px] leading-[1.55] text-text-secondary`) and its lead
    /// (`font-medium text-warning`).
    pub card_text: TextStyle,
    pub card_lead: TextStyle,
    pub warning: Hsla,
    /// The avatar's letter: weight 600, `max(10, floor(size × 0.42))` px.
    pub avatar_letter: TextStyle,
    pub reply_avatar_letter: TextStyle,
    /// The code block header's label: `font-channel text-[10.5px] font-semibold tracking-[0.07em]`.
    pub code_label: TextStyle,
    /// The expand bar: `font-mono text-[10.5px] tracking-wide`.
    pub code_foot: TextStyle,
    /// The placeholder caption inside a media box.
    pub caption: TextStyle,
    pub reply_line: f32,

    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
    pub accent_bright: Hsla,
    pub accent_soft: Hsla,
    pub error: Hsla,
    pub success: Hsla,
    pub row_hover: Hsla,
    pub border: Hsla,
    pub border_divider: Hsla,
    pub bg_secondary: Hsla,
    pub bg_darkest: Hsla,
    pub bg_light: Hsla,
    pub surface_hover: Hsla,
    pub surface_active: Hsla,
    pub spine: Hsla,
    pub av_fg: Hsla,
    pub scrollbar: Hsla,
    pub selection: Hsla,
    pub selection_text: Hsla,
    pub radius_sm: Pixels,
    pub radius_md: Pixels,
    pub radius_lg: Pixels,
    /// `rounded-avatar` as a share of the box (0 in themes with a fixed radius).
    pub avatar_radius: Pixels,
    pub reply_avatar_radius: Pixels,
}

fn half(color: Hsla) -> Hsla {
    Hsla { a: color.a * 0.5, ..color }
}

impl ChatStyle {
    pub fn new(scale: f32, cx: &mut App) -> Rc<ChatStyle> {
        let engine = Engine::global(cx);
        let theme = cx.theme();
        Rc::new(Self::build(&engine, theme, scale))
    }

    fn build(engine: &Engine, theme: &Theme, scale: f32) -> ChatStyle {
        let tracking = theme.palette.t_ls_body * BODY_FONT_SIZE;
        let face = |role: FontRole, weight: f32, italic: bool| -> Face {
            engine.face(&FontSpec { family: theme.font(role), weight: weight.round() as u16, italic })
        };
        // What a `Label` is: the size, 1.5 line height (Tailwind's preflight), the body tracking.
        let text = |role: FontRole, size: f32, weight: f32| -> TextStyle {
            let mut style = TextStyle::new(face(role, weight, false), size, theme.color_text_primary);
            style.line_height = LineHeight::Factor(1.5);
            style.letter_spacing = tracking;
            style
        };
        let scaled = &theme.scaled;
        let rich = RichStyle {
            family: theme.font(FontRole::Sans),
            mono_family: theme.font(FontRole::Mono),
            size: scaled.text_body,
            leading: theme.palette.t_lh_body,
            letter_spacing: tracking,
            color: theme.color_text_primary,
            link: theme.color_accent,
            muted: theme.color_text_muted,
            meta_size: scaled.text_meta,
            error: theme.color_error,
            code_background: theme.color_bg_darkest,
            code_radius: theme.radius_sm.into(),
        };
        let rich_pending = RichStyle {
            color: half(rich.color),
            link: half(rich.link),
            muted: half(rich.muted),
            error: half(rich.error),
            code_background: half(rich.code_background),
            ..rich.clone()
        };
        let mono = |italic| face(FontRole::Mono, 400., italic);
        let code = CodeStyle {
            text: TextStyle::new(mono(false), CODE_FONT, theme.color_text_primary).leading(1.5),
            italic: mono(true),
            keyword: theme.color_accent_bright,
            title: theme.color_accent,
            string: theme.color_success,
            number: theme.color_warning,
            comment: theme.color_text_muted,
            deletion: theme.color_error,
            operator: theme.color_text_secondary,
        };
        let time = text(FontRole::Meta, scaled.text_meta, 400.);
        let mut hover_time = time;
        hover_time.line_height = LineHeight::Factor(1.);
        let mut reply = text(FontRole::Sans, scaled.text_reply, 400.);
        reply.line_height = LineHeight::Px(scaled.text_reply_line_height);
        let mut reply_name = text(FontRole::Channel, scaled.text_reply, theme.palette.font_weight_emphasis);
        reply_name.line_height = reply.line_height;
        let mut notice = TextStyle::new(face(FontRole::Sans, 400., true), scaled.text_body, theme.color_text_muted)
            .leading(theme.palette.t_lh_body)
            .letter_spacing(tracking);
        notice.selectable = true;
        let mut card_text = text(FontRole::Sans, 13., 400.);
        card_text.line_height = LineHeight::Factor(1.55);
        card_text.color = theme.color_text_secondary;
        let mut card_lead = text(FontRole::Sans, 13., 500.);
        card_lead.line_height = LineHeight::Factor(1.55);
        card_lead.color = theme.color_warning;
        let letter = |size: f32| {
            let font = (size * 0.42).floor().max(10.);
            text(FontRole::Sans, font, 600.)
        };
        let mut code_label = text(FontRole::Channel, 10.5, 600.);
        code_label.letter_spacing = 0.07 * 10.5;
        // `tracking-wide`: 0.025em. The bar inherits the body's `leading-body`.
        let mut code_foot = TextStyle::new(mono(false), 10.5, theme.color_text_muted).leading(theme.palette.t_lh_body);
        code_foot.letter_spacing = 0.025 * 10.5;

        let mut hasher = KeyHasher::default();
        (theme.id.index(), scale.to_bits(), scaled.text_body.to_bits(), scaled.text_meta.to_bits()).hash(&mut hasher);
        (scaled.text_sender.to_bits(), scaled.text_reply.to_bits()).hash(&mut hasher);

        let muted = theme.color_text_muted;
        ChatStyle {
            key: hasher.finish(),
            scale,
            rich,
            rich_pending,
            code,
            math: MathColors { text: theme.color_text_primary, error: theme.color_error },
            name: text(FontRole::Channel, scaled.text_sender, theme.palette.font_weight_emphasis),
            time,
            hover_time,
            day: text(FontRole::Meta, scaled.text_meta, 500.),
            reply,
            reply_name,
            notice,
            card_text,
            card_lead,
            warning: theme.color_warning,
            avatar_letter: letter(AVATAR),
            reply_avatar_letter: letter(REPLY_AVATAR),
            code_label,
            code_foot,
            caption: text(FontRole::Meta, scaled.text_meta, 400.),
            reply_line: scaled.text_reply_line_height.max(REPLY_AVATAR),
            text_primary: theme.color_text_primary,
            text_secondary: theme.color_text_secondary,
            text_muted: muted,
            accent: theme.color_accent,
            accent_bright: theme.color_accent_bright,
            accent_soft: theme.color_accent_soft,
            error: theme.color_error,
            success: theme.color_success,
            row_hover: theme.color_row_hover,
            border: theme.color_border,
            border_divider: theme.color_border_divider,
            bg_secondary: theme.color_bg_secondary,
            bg_darkest: theme.color_bg_darkest,
            bg_light: theme.color_bg_light,
            surface_hover: theme.color_surface_hover,
            surface_active: theme.color_surface_active,
            // `border-text-muted/35`.
            spine: Hsla { a: muted.a * 0.35, ..muted },
            av_fg: theme.color_av_fg,
            scrollbar: theme.t_scrollbar,
            selection: gpui::rgb(SELECTION_BACKGROUND).into(),
            selection_text: gpui::rgb(SELECTION_TEXT).into(),
            radius_sm: theme.radius_sm,
            radius_md: theme.radius_md,
            radius_lg: theme.radius_lg,
            avatar_radius: theme.avatar_radius(px(AVATAR)),
            reply_avatar_radius: theme.avatar_radius(px(REPLY_AVATAR)),
        }
    }
}

/// Snaps a length to Blink's layout unit (1/64 px), so sums of row heights stay exact in an f64.
pub fn layout_unit(value: f32) -> f32 {
    (value * 64.).floor() / 64.
}
