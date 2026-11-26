//! The colours and radii the shell draws with, copied out of the theme once per render.
//!
//! `decibell_ui::Tokens` holds its shadows in `Vec`s and isn't `Clone`; holding `&Tokens`
//! borrows the app for the whole render. The shell's views need `&mut` access to the app
//! between reads, so they take this small `Copy` snapshot (the same field names as `Tokens`, so
//! code reads the same) and look shadows up where they are drawn.

use decibell_ui::{Theme, Tokens};
use gpui::{Hsla, Pixels};

macro_rules! ink {
    ($($field:ident)*) => {
        #[derive(Clone, Copy, Debug)]
        pub struct Ink {
            $(pub $field: Hsla,)*
            pub radius_sm: Pixels,
            pub radius_md: Pixels,
            pub radius_lg: Pixels,
            pub radius_xl: Pixels,
        }

        impl Ink {
            pub fn of(tokens: &Tokens, theme: &Theme) -> Ink {
                Ink {
                    $($field: tokens.$field,)*
                    radius_sm: theme.radius_sm,
                    radius_md: theme.radius_md,
                    radius_lg: theme.radius_lg,
                    radius_xl: theme.radius_xl,
                }
            }
        }
    };
}

ink! {
    color_bg_darkest color_bg_dark color_bg_light color_bg_primary
    color_bg_secondary color_border color_border_divider color_accent
    color_accent_bright color_accent_soft color_accent_mid
    color_on_accent color_success color_warning color_error color_text_primary
    color_text_bright color_text_secondary color_text_muted color_surface_hover color_surface_active
    color_av_fg
}

/// The chrome scope's ink (title bar, server bar, sidebar group).
pub fn chrome(cx: &gpui::App) -> Ink {
    use decibell_ui::ActiveTheme as _;
    let theme = cx.theme();
    Ink::of(&theme.chrome, theme)
}

/// The base scope's ink (the panes).
pub fn base(cx: &gpui::App) -> Ink {
    use decibell_ui::ActiveTheme as _;
    let theme = cx.theme();
    Ink::of(theme, theme)
}
