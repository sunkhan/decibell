//! `text-roles`: what `Label` draws.

use decibell_ui::{ActiveTheme as _, FontRole, Label};
use gpui::{Context, Div, IntoElement, Render, Window, div, prelude::*, px};

const PANGRAM: &str = "Sphinx of black quartz, judge my vow: 0123456789 ığüşöç ÀÉÎõü";
const LONG: &str = "A channel topic that goes on for longer than its box allows, so that it has to be cut \
    somewhere, with an ellipsis where the text stops being shown to the reader of the row.";

pub struct TextRoles;

impl Render for TextRoles {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.color_text_muted;
        let section = |title: &'static str| {
            Label::new(title)
                .font(FontRole::Section)
                .size(11.)
                .weight(600.)
                .tracking(0.12)
                .color(muted)
        };
        let caption = |text: String| div().w(px(132.)).flex_none().child(Label::new(text).size(11.).color(muted).nowrap());
        let row = || div().flex().items_baseline().gap_3();

        let roles = [
            ("sans", FontRole::Sans),
            ("meta", FontRole::Meta),
            ("mono", FontRole::Mono),
            ("channel", FontRole::Channel),
            ("display", FontRole::Display),
            ("section", FontRole::Section),
            ("tile", FontRole::Tile),
        ];
        let mut role_rows = div().flex().flex_col().gap_1();
        for (name, role) in roles {
            for (size, weight) in [(11.5, 400.), (13., 500.), (14.5, 400.), (16., 600.), (20., 700.)] {
                role_rows = role_rows.child(
                    row()
                        .child(caption(format!("{name} {size} / {weight}")))
                        .child(Label::new(PANGRAM).font(role).size(size).weight(weight).nowrap()),
                );
            }
        }

        let mut spacing = div().flex().flex_col().gap_1();
        for (name, em) in [("-0.02em", -0.02), ("0", 0.), ("0.07em", 0.07), ("0.12em", 0.12), ("0.24em", 0.24)] {
            spacing = spacing.child(
                row()
                    .child(caption(format!("tracking {name}")))
                    .child(Label::new("TEXT CHANNELS · Voice connected").size(12.).weight(600.).tracking(em).nowrap()),
            );
        }
        spacing = spacing
            .child(row().child(caption("italic 400".into())).child(Label::new(PANGRAM).italic().nowrap()))
            .child(row().child(caption("italic 700".into())).child(Label::new(PANGRAM).italic().weight(700.).nowrap()))
            .child(row().child(caption("mono italic".into())).child(Label::new("// a comment, slanted").font(FontRole::Mono).size(12.5).italic().nowrap()));

        let boxed = |width: f32, child: Label| -> Div {
            div()
                .w(px(width))
                .flex_none()
                .p_2()
                .rounded(theme.radius_sm)
                .bg(theme.color_bg_secondary)
                .child(child)
        };
        let mut cuts = div().flex().flex_wrap().items_start().gap_3();
        for width in [120., 200., 320.] {
            cuts = cuts
                .child(boxed(width, Label::new(LONG).size(13.).truncate()))
                .child(boxed(width, Label::new(LONG).size(13.).clamp(2)))
                .child(boxed(width, Label::new(LONG).size(13.).clamp(3)));
        }
        let wrapped = div()
            .flex()
            .items_start()
            .gap_3()
            .child(boxed(240., Label::new(LONG)))
            .child(boxed(400., Label::new(LONG).font(FontRole::Meta).size(12.).line_height(22.)))
            .child(boxed(240., Label::new("Supercalifragilisticexpialidocious-pneumonoultramicroscopic words overflow")));

        div()
            .id("text-roles")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.color_bg_primary)
            .text_color(theme.color_text_primary)
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(section("ROLES"))
            .child(role_rows)
            .child(section("LETTER-SPACING AND SLANT"))
            .child(spacing)
            .child(section("TRUNCATE · CLAMP 2 · CLAMP 3 AT 120 / 200 / 320"))
            .child(cuts)
            .child(section("WRAPPED"))
            .child(wrapped)
    }
}
