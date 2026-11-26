//! The colours, radii and sizes the composer draws with, copied out of the theme once per render
//! (the shell's `ink` pattern): `&Theme` borrows the app for the whole render, and the composer's
//! renders need `&mut` access between reads (listeners, other entities). Shadows are read from
//! the theme where they are drawn.

use decibell_ui::ActiveTheme as _;
use gpui::{App, Hsla, Pixels};

macro_rules! ink {
    ($($field:ident)*) => {
        #[derive(Clone, Copy, Debug)]
        pub struct Ink {
            $(pub $field: Hsla,)*
            pub radius_sm: Pixels,
            pub radius_md: Pixels,
            pub radius_lg: Pixels,
            /// `--text-meta` (px).
            pub text_meta: f32,
        }

        impl Ink {
            pub fn of(cx: &App) -> Ink {
                let theme = cx.theme();
                Ink {
                    $($field: theme.$field,)*
                    radius_sm: theme.radius_sm,
                    radius_md: theme.radius_md,
                    radius_lg: theme.radius_lg,
                    text_meta: theme.scaled.text_meta,
                }
            }
        }
    };
}

ink! {
    color_bg_darkest color_bg_mid color_bg_light color_bg_lighter color_border
    color_border_divider color_accent color_accent_hover color_accent_soft
    color_on_accent color_error color_text_primary color_text_secondary color_text_muted
    color_surface_hover
}
