//! The active theme as a GPUI global.
//!
//! `decibell-theme` holds the tokens as plain data; this is the same data in GPUI's types,
//! converted once when the theme (or the text size / density) changes, never while painting.
//!
//! ```ignore
//! use decibell_ui::ActiveTheme as _;
//! let theme = cx.theme();
//! div().bg(theme.color_bg_primary).rounded(theme.radius_md).shadow(theme.shadow_raised.clone())
//! // inside the title bar, server bar and sidebars (`.chrome-scope` in the CSS):
//! div().bg(theme.chrome.color_bg_titlebar)
//! ```

use std::ops::Deref;

use decibell_theme::{
    Color, DEFAULT_ROW_SCALE, DEFAULT_TEXT_SIZE_PX, LengthPct, Palette, Scaled, ShadowLayer,
    ThemeId, clamp_row_scale, clamp_text_size_px, scaled,
};
use gpui::{App, BoxShadow, Global, Hsla, IntoElement, Pixels, Rgba, SharedString, px};

use crate::fonts;

/// Declares [`Tokens`]: every colour of a [`Palette`] under the same field name, as `Hsla`.
/// The test below fails if the list and the palette's colour tokens ever differ.
macro_rules! tokens {
    ($($field:ident)*) => {
        /// The paint-time tokens of one scope (the base theme, or its chrome): every colour
        /// of the [`Palette`] under the same name as an `Hsla`, and the elevation shadows.
        pub struct Tokens {
            $(pub $field: Hsla,)*
            /// `shadow-raised`: cards and buttons that sit on the surface.
            pub shadow_raised: Vec<BoxShadow>,
            /// `shadow-float`: menus, popovers, tooltips.
            pub shadow_float: Vec<BoxShadow>,
            /// `shadow-modal`: dialogs and windows over a backdrop.
            pub shadow_modal: Vec<BoxShadow>,
            /// `shadow-ring`: the focus ring.
            pub shadow_ring: Vec<BoxShadow>,
            /// `shadow-glow`: accent glow.
            pub shadow_glow: Vec<BoxShadow>,
        }

        impl Tokens {
            fn new(palette: &Palette) -> Self {
                Self {
                    $($field: hsla(palette.$field),)*
                    shadow_raised: box_shadows(palette.shadow_raised),
                    shadow_float: box_shadows(palette.shadow_float),
                    shadow_modal: box_shadows(palette.shadow_modal),
                    shadow_ring: box_shadows(palette.shadow_ring),
                    shadow_glow: box_shadows(palette.shadow_glow),
                }
            }

            #[cfg(test)]
            const COLOR_FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];
        }
    };
}

tokens! {
    color_bg_darkest color_bg_dark color_bg_mid color_bg_light color_bg_lighter color_bg_surface
    color_bg_titlebar color_bg_primary color_bg_secondary color_bg_tertiary color_bg_input
    color_bg_dmbar color_border color_border_divider color_accent color_accent_bright
    color_accent_hover color_accent_pressed color_accent_soft color_accent_mid color_on_accent
    color_success color_warning color_error color_danger color_on_error color_live
    color_text_primary color_text_bright color_text_secondary color_text_muted color_text_faint
    color_surface_hover color_surface_active color_row_hover t_live color_av_fg color_av_1
    color_av_2 color_av_3 color_av_4 color_av_5 color_av_6 color_av_7 color_av_8 color_name_1
    color_name_2 color_name_3 color_name_4 color_name_5 color_name_6 color_name_7 color_name_8
    t_chrome t_sidebar t_content t_raised t_high t_surface t_input t_border t_divider
    t_surface_hover t_surface_active t_row_hover t_accent t_accent_bright t_accent_hover
    t_accent_pressed t_accent_soft t_accent_mid t_on_accent t_success t_warning t_error
    t_on_error t_text_primary t_text_bright t_text_secondary t_text_muted t_text_faint
    t_scrollbar t_scrollbar_hover t_av_fg t_av_1 t_av_2 t_av_3 t_av_4 t_av_5 t_av_6 t_av_7
    t_av_8 t_name_1 t_name_2 t_name_3 t_name_4 t_name_5 t_name_6 t_name_7 t_name_8
}

/// Which face of the theme a piece of text is set in (`font-sans`, `font-meta`, … in the CSS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    /// UI and prose.
    Sans,
    /// Timestamps, counts, readouts.
    Meta,
    /// Code, fingerprints, key caps.
    Mono,
    /// Channel and member rows, the title bar.
    Channel,
    /// Headers.
    Display,
    /// Uppercase group labels.
    Section,
    /// Server tiles: the tile family at the theme's tile width.
    Tile,
    /// The wordmark: the tile family at the theme's widest cut.
    Wordmark,
}

/// The active theme. Read it with [`ActiveTheme::theme`]; change it with [`Theme::set`].
///
/// Derefs to the base scope's [`Tokens`], so `cx.theme().color_bg_primary` is the colour and
/// `cx.theme().chrome.color_bg_primary` the same token inside the chrome.
pub struct Theme {
    pub id: ThemeId,
    /// The raw tokens: numbers, font families, anything [`Tokens`] doesn't carry.
    pub palette: &'static Palette,
    tokens: Tokens,
    /// The tokens inside `.chrome-scope`: title bar, server bar, sidebars, image viewer.
    /// The same values as the base scope except in console-split.
    pub chrome: Tokens,
    /// The type scale at the user's text size and list density.
    pub scaled: Scaled,
    pub radius_sm: Pixels,
    pub radius_md: Pixels,
    pub radius_lg: Pixels,
    pub radius_xl: Pixels,
    text_size: f32,
    row_scale: f32,
    fonts: [SharedString; 8],
    /// The same theme seen from inside `.chrome-scope` (its base tokens are the chrome's): what
    /// [`ActiveTheme::theme`] answers inside [`chrome_scope`]. `None` on that copy itself.
    chrome_view: Option<Box<Theme>>,
}

impl Deref for Theme {
    type Target = Tokens;

    fn deref(&self) -> &Tokens {
        &self.tokens
    }
}

impl Global for Theme {}

impl Theme {
    fn new(id: ThemeId, text_size: f32, row_scale: f32) -> Self {
        let mut theme = Self::scoped(id, id.palette(), text_size, row_scale);
        // In the chrome every token is the chrome's (console-split is the only theme where the
        // two differ).
        theme.chrome_view = Some(Box::new(Self::scoped(id, id.chrome_palette(), text_size, row_scale)));
        theme
    }

    fn scoped(id: ThemeId, palette: &'static Palette, text_size: f32, row_scale: f32) -> Self {
        let family = |token, stretch| SharedString::new_static(fonts::family(token, stretch));
        Self {
            id,
            palette,
            tokens: Tokens::new(palette),
            chrome: Tokens::new(id.chrome_palette()),
            scaled: scaled(palette, text_size, row_scale),
            radius_sm: px(palette.radius_sm),
            radius_md: px(palette.radius_md),
            radius_lg: px(palette.radius_lg),
            radius_xl: px(palette.radius_xl),
            text_size,
            row_scale,
            // Indexed by `FontRole as usize`.
            fonts: [
                family(palette.font_sans, 100.),
                family(palette.font_meta, 100.),
                family(palette.font_mono, 100.),
                family(palette.font_channel, 100.),
                family(palette.font_display, 100.),
                family(palette.font_section, 100.),
                family(palette.font_tile, palette.t_tile_stretch),
                family(palette.font_tile, palette.t_wordmark_stretch),
            ],
            chrome_view: None,
        }
    }

    /// Installs the theme global. Call once at startup, before any window opens.
    pub fn init(cx: &mut App, id: ThemeId) {
        cx.set_global(Theme::new(id, DEFAULT_TEXT_SIZE_PX, DEFAULT_ROW_SCALE));
    }

    /// Switches theme and repaints every window.
    pub fn set(cx: &mut App, id: ThemeId) {
        let (text_size, row_scale) = (cx.theme().text_size, cx.theme().row_scale);
        Self::replace(cx, Theme::new(id, text_size, row_scale));
    }

    /// Settings → Appearance: the message text size in px and the list density multiplier
    /// (both clamped to the renderer's ranges).
    pub fn set_scale(cx: &mut App, text_size: f32, row_scale: f32) {
        let id = cx.theme().id;
        Self::replace(
            cx,
            Theme::new(
                id,
                clamp_text_size_px(text_size),
                clamp_row_scale(row_scale),
            ),
        );
    }

    fn replace(cx: &mut App, theme: Theme) {
        cx.set_global(theme);
        cx.refresh_windows();
    }

    /// The family name to hand GPUI for `role` in this theme.
    pub fn font(&self, role: FontRole) -> SharedString {
        self.fonts[role as usize].clone()
    }

    /// `rounded-avatar` for a box of `size`: a share of the box in most themes, a fixed
    /// radius in the console ones.
    pub fn avatar_radius(&self, size: Pixels) -> Pixels {
        match self.palette.radius_avatar {
            LengthPct::Px(radius) => px(radius),
            LengthPct::Fraction(fraction) => size * fraction,
        }
    }
}

/// `cx.theme()` on anything that derefs to [`App`].
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        let theme = self.global::<Theme>();
        if CHROME_SCOPE.with(|depth| depth.get() > 0) {
            return theme.chrome_view.as_deref().unwrap_or(theme);
        }
        theme
    }
}

thread_local! {
    static CHROME_SCOPE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// CSS's `.chrome-scope` for whatever `child` draws: while it is laid out, prepainted and painted,
/// `cx.theme()` answers the chrome's tokens as the base ones, so kit components inside (a
/// confirmation, a menu, their buttons) take the sidebar's palette as the renderer's do when they
/// are rendered inside the sidebar's DOM (console-split is the only theme where it differs). Views
/// that draw the chrome themselves keep reading `theme.chrome` explicitly.
pub fn chrome_scope(child: impl IntoElement) -> ChromeScope {
    ChromeScope { child: Some(child.into_any_element()) }
}

pub struct ChromeScope {
    child: Option<gpui::AnyElement>,
}

pub(crate) struct InChrome;

/// Inside a [`chrome_scope`] now (a layer built here keeps it for its deferred draw).
pub(crate) fn in_chrome() -> bool {
    CHROME_SCOPE.with(|depth| depth.get() > 0)
}

impl InChrome {
    pub(crate) fn enter() -> Self {
        CHROME_SCOPE.with(|depth| depth.set(depth.get() + 1));
        InChrome
    }
}

impl Drop for InChrome {
    fn drop(&mut self) {
        CHROME_SCOPE.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

impl gpui::Element for ChromeScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        let _scope = InChrome::enter();
        (self.child.as_mut().expect("the scope's content").request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        _: &mut (),
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        let _scope = InChrome::enter();
        if let Some(child) = &mut self.child {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        let _scope = InChrome::enter();
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// A theme colour as GPUI paints it.
pub fn hsla(color: Color) -> Hsla {
    let [r, g, b, a] = color.to_rgba_f32();
    Rgba { r, g, b, a }.into()
}

/// A CSS `box-shadow` token as GPUI shadows.
///
/// Two differences from CSS are handled here: GPUI's `blur_radius` is the Gaussian's standard
/// deviation where CSS gives twice that, and GPUI paints the list first-at-the-bottom where
/// CSS paints the first layer on top.
pub fn box_shadows(layers: &[ShadowLayer]) -> Vec<BoxShadow> {
    layers
        .iter()
        .rev()
        .map(|layer| BoxShadow {
            color: hsla(layer.color),
            offset: gpui::point(px(layer.x), px(layer.y)),
            blur_radius: px(layer.blur / 2.),
            spread_radius: px(layer.spread),
            inset: layer.inset,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use decibell_theme::TokenValue;

    #[test]
    fn tokens_cover_every_colour_of_the_palette() {
        let in_palette: Vec<String> = ThemeId::Nocturne
            .palette()
            .tokens()
            .into_iter()
            .filter(|(_, value)| matches!(value, TokenValue::Color(_)))
            .map(|(css, _)| css.trim_start_matches("--").replace('-', "_"))
            .collect();
        assert_eq!(Tokens::COLOR_FIELDS, in_palette);
    }

    #[test]
    fn colours_convert_exactly() {
        let color = hsla(Color(0x3366_9980));
        let back: Rgba = color.into();
        for (got, want) in [
            (back.r, 0x33),
            (back.g, 0x66),
            (back.b, 0x99),
            (back.a, 0x80),
        ] {
            assert!((got * 255. - want as f32).abs() < 0.01, "{got} vs {want}");
        }
    }

    #[test]
    fn shadows_follow_css() {
        // nocturne's shadow-float: a blurred drop shadow, a 1px ring, an inset highlight.
        let css = ThemeId::Nocturne.palette().shadow_float;
        let shadows = box_shadows(css);
        assert_eq!(shadows.len(), css.len());
        let (top, bottom) = (shadows.last().unwrap(), &shadows[0]);
        assert_eq!(top.blur_radius, px(css[0].blur / 2.));
        assert_eq!(top.offset.y, px(css[0].y));
        assert!(bottom.inset && css.last().unwrap().inset);
    }

    #[test]
    fn every_theme_builds_and_names_bundled_families() {
        for id in ThemeId::ALL {
            let theme = Theme::new(id, DEFAULT_TEXT_SIZE_PX, DEFAULT_ROW_SCALE);
            for family in &theme.fonts {
                assert!(
                    fonts::FACES
                        .iter()
                        .any(|face| face.family == family.as_ref()),
                    "{id}: {family} is not a bundled family"
                );
            }
            assert!(theme.scaled.text_body > 0.);
        }
        let split = Theme::new(ThemeId::ConsoleSplit, 14.5, 1.0);
        assert_ne!(split.color_bg_primary, split.chrome.color_bg_primary);
    }
}
