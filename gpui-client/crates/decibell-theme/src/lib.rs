//! Decibell's design tokens for the native client.
//!
//! Everything token-shaped is generated from the Electron renderer's
//! `electron-client/src/styles/globals.css` by `gpui-client/tools/gen-theme/gen-theme.mjs` into
//! `generated.rs` (never edit it; `gpui-client/tools/gen-theme/check.sh` fails while it is stale). This file holds
//! the hand-written types the generated data is made of, plus lookups by [`ThemeId`].
//!
//! - [`Palette`]: every custom property of one theme, resolved as Chromium resolves it on `<html data-theme=…>`.
//!   Field names are the CSS names snake-cased (`--color-bg-mid` → `color_bg_mid`), so a token greps both ways.
//!   Both layers are there: the `--t-*` raw palette and the `--color-*` / `--radius-*` / `--font-*` names the
//!   Tailwind utilities read (`bg-bg-mid` → `color_bg_mid`, `rounded-md` → `radius_md`, `font-meta` → `font_meta`,
//!   `shadow-float` → `shadow_float`, `tracking-section` → `tracking_section`, `leading-body` → `leading_body`).
//! - [`chrome_palette`]: the palette inside `.chrome-scope` (title bar, server bar, sidebars, image viewer). The same
//!   `&'static Palette` as [`palette`] except in console-split, whose chrome is console's dark ramp around a light
//!   canvas. As in CSS, only `--color-*` (and `--t-shadow-*` / `shadow_*`) are re-pointed there: `t_chrome`,
//!   `t_scrollbar`, radii, fonts keep the base theme's values.
//! - [`scaled`]: the type scale at the user's text size and list density (Settings → Appearance).
//! - [`scopes`]: component-local tokens (`.image-viewer`, `.picture-editor`, `.dsel-menu`, `.theme-preview`).
//! - [`tailwind`]: Tailwind's default theme for the utilities that aren't themed (`px-4` = 4 × `SPACING`, `text-sm`,
//!   `leading-snug`, `ease-out`, the `lg:` breakpoint, …).
//!
//! What globals.css does that is *not* a token (component rules, base text styles, keyframes, scrollbars) is
//! listed with line ranges in `gpui-client/docs/css-component-rules.md`.
//!
//! # Text, as Chromium sets it
//!
//! - Families: the `font_*` fields name the bundled family (`"Inter"`, `"Mona Sans"`, `"JetBrains Mono"`); the
//!   renderer loads no italic face (Chromium slants the upright one) and no other family. Roles: `font_sans`
//!   (UI and prose), `font_meta` (timestamps, counts, readouts), `font_mono` (code, fingerprints, `<kbd>`;
//!   JetBrains Mono in every theme), `font_channel` (channel/member rows), `font_display` (headers), `font_section`
//!   (uppercase group labels), `font_tile` (server tiles; with `t_tile_stretch`), the wordmark's stretch
//!   `t_wordmark_stretch`.
//! - Sizes: the `text_*` roles of [`Scaled`] (`text-body` → `text_body`); Tailwind's fixed steps (`text-sm`) in
//!   [`tailwind`]; arbitrary `text-[13px]` in the TSX are literal px and do not follow the text-size setting.
//! - Line height: `leading_body` for message prose, `text_reply_line_height` (whole px) for reply lines, Tailwind's
//!   `--text-*--line-height` with its size, else the inherited 1.5 (Tailwind preflight on `<html>`). A CSS
//!   line-height is a multiple of the font size, not of the font's own line gap.
//! - Weight: 400 for body text (globals.css `body`), labels 500, emphasis `font_weight_emphasis` (600, console 700).
//! - Letter-spacing (em): body `t_ls_body`, `tracking_title` / `tracking_section` / `tracking_wordmark`, headings
//!   -0.02em, Tailwind's `tracking-*` in [`tailwind`].
//! - Axes: the renderer's Inter is fontsource's wght-only cut, i.e. the design at `opsz` 14 at every size
//!   (`font-optical-sizing` has no axis to drive); the bundled `InterVariable.ttf` also has `opsz` 14–32, so hold
//!   it at 14 (CoreText applies optical sizing on its own unless pinned). Mona Sans `wdth` = the font-stretch %
//!   (`t_tile_stretch` 112.5, `t_wordmark_stretch` 125, else 100) and its default instance is wght 200, so always
//!   pass the weight. JetBrains Mono is bundled as the four static weights 400–700 the renderer loads.
//! - OpenType features: globals.css asks for `cv11`, `ss01`, `ss03` on `body`, but the renderer's fontsource
//!   subsets don't contain them (Inter's GSUB there: calt ccmp dnom frac locl numr pnum tnum), so they change
//!   nothing in the renderer. The bundled full fonts do contain them: leave them off to render the same.

use std::fmt;
use std::str::FromStr;

#[rustfmt::skip]
mod generated;
#[cfg(test)]
#[rustfmt::skip]
mod generated_tests;

pub use generated::*;

/// An sRGB colour with straight (not premultiplied) alpha, as `0xRRGGBBAA` — the layout of gpui's `rgba(u32)`.
///
/// CSS alphas are quantised to 8 bits when generated (`rgba(…, 0.09)` → `0x17`, 22.95 rounded), which is the only
/// rounding between the CSS and these values.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Color = Color(0);
    pub const BLACK: Color = Color(0x0000_00ff);
    pub const WHITE: Color = Color(0xffff_ffff);

    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color((r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32)
    }
    /// An opaque colour from `0xRRGGBB` (CSS `#rrggbb`).
    pub const fn from_rgb(rgb: u32) -> Color {
        Color(rgb << 8 | 0xff)
    }
    pub const fn r(self) -> u8 {
        (self.0 >> 24) as u8
    }
    pub const fn g(self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub const fn b(self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub const fn a(self) -> u8 {
        self.0 as u8
    }
    /// Alpha as 0–1.
    pub const fn alpha(self) -> f32 {
        self.a() as f32 / 255.0
    }
    /// The same colour at `alpha` (0–1, clamped; NaN reads as 0), rounded to 8 bits.
    pub const fn with_alpha(self, alpha: f32) -> Color {
        Color((self.0 & 0xffff_ff00) | unit_to_u8(alpha) as u32)
    }
    /// The alpha multiplied by `factor`. `color-mix(in srgb, c p%, transparent)` is `c.scale_alpha(p)` in any space.
    pub const fn scale_alpha(self, factor: f32) -> Color {
        self.with_alpha(self.alpha() * factor)
    }
    /// `[r, g, b, a]` in 0–1 (gpui's `Rgba`).
    pub const fn to_rgba_f32(self) -> [f32; 4] {
        [
            self.r() as f32 / 255.0,
            self.g() as f32 / 255.0,
            self.b() as f32 / 255.0,
            self.alpha(),
        ]
    }
    /// `[h, s, l, a]`, each in 0–1 (gpui's `Hsla` / `hsla(h, s, l, a)`: hue as a fraction of a turn).
    pub fn to_hsla(self) -> [f32; 4] {
        let [r, g, b, a] = self.to_rgba_f32().map(|c| c as f64);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return [0.0, 0.0, l as f32, a as f32];
        }
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } / 6.0;
        [h as f32, s as f32, l as f32, a as f32]
    }
    /// CSS `color-mix(in srgb, self <weight×100>%, other)`: premultiplied interpolation in sRGB, rounded to 8 bits
    /// (the same arithmetic the generator applies to the color-mix() tokens). `weight` is clamped to 0–1.
    pub fn mix(self, other: Color, weight: f32) -> Color {
        let p = (weight as f64).clamp(0.0, 1.0);
        let q = 1.0 - p;
        let [r1, g1, b1, a1] = self.to_rgba_f32().map(|c| c as f64);
        let [r2, g2, b2, a2] = other.to_rgba_f32().map(|c| c as f64);
        let alpha = a1 * p + a2 * q;
        if alpha == 0.0 {
            return Color::TRANSPARENT;
        }
        let ch = |c1: f64, c2: f64| {
            ((c1 * a1 * p + c2 * a2 * q) / alpha * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color::from_rgba8(
            ch(r1, r2),
            ch(g1, g2),
            ch(b1, b2),
            (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }
}

const fn unit_to_u8(x: f32) -> u8 {
    if x.is_nan() || x <= 0.0 {
        0
    } else if x >= 1.0 {
        255
    } else {
        (x * 255.0 + 0.5) as u8
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:08x}", self.0)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:08x}", self.0)
    }
}

/// One layer of a CSS `box-shadow`. Lengths in px; `blur` is the CSS blur radius (a Gaussian of σ = blur / 2).
/// A token's layers are listed as in CSS: the first paints on top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowLayer {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

/// A length that is px in some themes and a percentage of the box in others (`--radius-avatar`: 26% in the Mona
/// Sans themes, `var(--t-radius-md)` = 4px in console).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthPct {
    Px(f32),
    /// A CSS percentage as a fraction (26% → 0.26).
    Fraction(f32),
}

impl LengthPct {
    /// The length in px against `basis` (for a border radius: the box's side; avatars are square).
    pub fn resolve(self, basis: f32) -> f32 {
        match self {
            LengthPct::Px(px) => px,
            LengthPct::Fraction(f) => f * basis,
        }
    }
}

/// One CSS `filter` function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterFn {
    /// `blur(px)`
    Blur(f32),
    /// `saturate(n)` (1 = unchanged)
    Saturate(f32),
    /// `brightness(n)` (1 = unchanged)
    Brightness(f32),
}

/// A CSS timing function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    /// `cubic-bezier(x1, y1, x2, y2)`
    CubicBezier(f32, f32, f32, f32),
}

/// A token's value, as [`Palette::tokens`] lists them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenValue {
    Color(Color),
    /// px, em, a unitless number, a fraction, ms or a wdth value; the field's doc says which.
    Number(f32),
    /// A bundled font family name.
    Family(&'static str),
    Shadow(&'static [ShadowLayer]),
    LengthPct(LengthPct),
    Filter(&'static [FilterFn]),
    Easing(Easing),
}

/// How the theme picker labels a theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    Dark,
    Light,
}

/// A theme, as `data-theme` names it and the settings file stores it (`theme: "graphite-light"`).
/// The order is Settings → Appearance's, and indexes [`THEMES`], [`CHROME_PALETTES`], [`SCOPES`], [`THEME_MODES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThemeId {
    #[default]
    Nocturne,
    Matinee,
    Graphite,
    GraphiteLight,
    Console,
    ConsoleLight,
    ConsoleSplit,
}

impl ThemeId {
    pub const ALL: [ThemeId; 7] = [
        ThemeId::Nocturne,
        ThemeId::Matinee,
        ThemeId::Graphite,
        ThemeId::GraphiteLight,
        ThemeId::Console,
        ThemeId::ConsoleLight,
        ThemeId::ConsoleSplit,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }
    /// The `data-theme` id, which is also the persisted setting value.
    pub const fn as_str(self) -> &'static str {
        match self {
            ThemeId::Nocturne => "nocturne",
            ThemeId::Matinee => "matinee",
            ThemeId::Graphite => "graphite",
            ThemeId::GraphiteLight => "graphite-light",
            ThemeId::Console => "console",
            ThemeId::ConsoleLight => "console-light",
            ThemeId::ConsoleSplit => "console-split",
        }
    }
    /// A persisted setting value; anything unknown falls back to the default theme, as the renderer's
    /// `loadSettings` does.
    pub fn from_setting(value: &str) -> ThemeId {
        value.parse().unwrap_or_default()
    }
    /// The name Settings → Appearance shows.
    pub fn display_name(self) -> &'static str {
        THEMES[self.index()].1
    }
    pub fn mode(self) -> ThemeMode {
        THEME_MODES[self.index()]
    }
    pub fn palette(self) -> &'static Palette {
        THEMES[self.index()].2
    }
    pub fn chrome_palette(self) -> &'static Palette {
        CHROME_PALETTES[self.index()]
    }
    pub fn scopes(self) -> &'static Scopes {
        &SCOPES[self.index()]
    }
}

/// An id that is not one of the seven themes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTheme(pub String);

impl fmt::Display for UnknownTheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown theme {:?}", self.0)
    }
}

impl std::error::Error for UnknownTheme {}

impl FromStr for ThemeId {
    type Err = UnknownTheme;
    fn from_str(s: &str) -> Result<ThemeId, UnknownTheme> {
        ThemeId::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| UnknownTheme(s.to_owned()))
    }
}

impl fmt::Display for ThemeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The theme's palette (`<html data-theme=…>`).
pub fn palette(id: ThemeId) -> &'static Palette {
    id.palette()
}

/// The theme's palette inside `.chrome-scope`.
pub fn chrome_palette(id: ThemeId) -> &'static Palette {
    id.chrome_palette()
}

/// The theme's component-local scopes.
pub fn scopes(id: ThemeId) -> &'static Scopes {
    id.scopes()
}

/// The persisted text size as the renderer applies it (uiStore `clampTextSizePx`): 0, negative or NaN (a config
/// from before the setting) is the default; otherwise snapped to the 0.5 px grid and clamped to 11–17.
pub fn clamp_text_size_px(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 || value.is_infinite() {
        return DEFAULT_TEXT_SIZE_PX;
    }
    let step = TEXT_SIZE_STEP_PX as f64;
    let snapped = js_round(value as f64 / step) * step;
    snapped.clamp(TEXT_SIZE_MIN_PX as f64, TEXT_SIZE_MAX_PX as f64) as f32
}

/// The persisted list density as the renderer applies it (uiStore `clampScale`): clamped, then rounded to 2 dp.
pub fn clamp_row_scale(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_ROW_SCALE;
    }
    let clamped = (value as f64).clamp(ROW_SCALE_MIN as f64, ROW_SCALE_MAX as f64);
    (js_round(clamped * 100.0) / 100.0) as f32
}

/// JavaScript's `Math.round`: halves go towards +∞.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// A CSS `<rounding-strategy>` for [`css_round`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingStrategy {
    /// `round(a, b)` / `round(nearest, a, b)`: ties go to the upper multiple (towards +∞).
    Nearest,
    /// `round(up, a, b)`: the multiple towards +∞.
    Up,
    /// `round(down, a, b)`: the multiple towards −∞.
    Down,
    /// `round(to-zero, a, b)`.
    ToZero,
}

/// CSS `round(<strategy>, a, b)`: `a` rounded to a multiple of `b`, as Chromium evaluates it (in f64). For a TSX
/// inline calc such as MembersList's `round(calc(…), 1px)`, pass `b = 1.0`.
pub fn css_round(strategy: RoundingStrategy, a: f64, b: f64) -> f64 {
    if b == 0.0 || a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let b = b.abs();
    if a % b == 0.0 {
        return a;
    }
    let lower = (a / b).floor() * b;
    let upper = lower + b;
    match strategy {
        RoundingStrategy::Up => upper,
        RoundingStrategy::Down => lower,
        RoundingStrategy::ToZero => {
            if a < 0.0 {
                upper
            } else {
                lower
            }
        }
        RoundingStrategy::Nearest => {
            if (a - lower).abs() < (upper - a).abs() {
                lower
            } else {
                upper
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILIES: [&str; 3] = ["Inter", "Mona Sans", "JetBrains Mono"];

    fn all_palettes() -> Vec<(&'static str, &'static Palette)> {
        let mut v: Vec<_> = THEMES.iter().map(|t| (t.0, t.2)).collect();
        v.push(("console-split (chrome)", &CONSOLE_SPLIT_CHROME));
        v
    }

    #[test]
    fn every_theme_resolves_every_field() {
        let reference: Vec<&str> = NOCTURNE.tokens().iter().map(|t| t.0).collect();
        let unique: std::collections::HashSet<_> = reference.iter().collect();
        assert_eq!(unique.len(), reference.len(), "duplicate token names");
        assert!(reference.len() > 150, "only {} tokens", reference.len());
        for (name, p) in all_palettes() {
            let tokens = p.tokens();
            assert_eq!(
                tokens.iter().map(|t| t.0).collect::<Vec<_>>(),
                reference,
                "{name}"
            );
            for (css, v) in tokens {
                match v {
                    TokenValue::Number(n) => assert!(n.is_finite(), "{name} {css} = {n}"),
                    TokenValue::Family(f) => assert!(FAMILIES.contains(&f), "{name} {css} = {f:?}"),
                    TokenValue::Shadow(layers) => {
                        assert!(!layers.is_empty(), "{name} {css}: no layers");
                        for l in layers {
                            assert!(
                                [l.x, l.y, l.blur, l.spread].iter().all(|x| x.is_finite()),
                                "{name} {css}"
                            );
                            assert!(l.blur >= 0.0 && l.color.a() > 0, "{name} {css}: {l:?}");
                        }
                    }
                    TokenValue::LengthPct(LengthPct::Px(x) | LengthPct::Fraction(x)) => {
                        assert!(x.is_finite() && x >= 0.0, "{name} {css}")
                    }
                    TokenValue::Filter(f) => assert!(!f.is_empty(), "{name} {css}"),
                    TokenValue::Color(_) | TokenValue::Easing(_) => {}
                }
            }
        }
    }

    #[test]
    fn theme_ids_match_the_generated_tables() {
        for (i, id) in ThemeId::ALL.into_iter().enumerate() {
            assert_eq!(id.index(), i);
            assert_eq!(id.as_str(), THEMES[i].0);
            assert_eq!(id.as_str().parse::<ThemeId>(), Ok(id));
            assert!(std::ptr::eq(palette(id), THEMES[i].2));
        }
        assert_eq!(ThemeId::default().as_str(), DEFAULT_THEME_ID);
        assert_eq!(
            ThemeId::from_setting("console-light"),
            ThemeId::ConsoleLight
        );
        assert_eq!(ThemeId::from_setting("solarized"), ThemeId::Nocturne);
        assert_eq!(ThemeId::from_setting(""), ThemeId::Nocturne);
        assert!("Console".parse::<ThemeId>().is_err());
        assert_eq!(ThemeId::GraphiteLight.display_name(), "Graphite Light");
        assert_eq!(ThemeId::ConsoleSplit.mode(), ThemeMode::Light);
        assert_eq!(ThemeId::Console.mode(), ThemeMode::Dark);
        // Only console-split has a chrome palette of its own.
        for id in ThemeId::ALL {
            assert_eq!(
                std::ptr::eq(chrome_palette(id), palette(id)),
                id != ThemeId::ConsoleSplit,
                "{id}"
            );
        }
    }

    #[test]
    fn modes_agree_with_the_canvas() {
        for id in ThemeId::ALL {
            let l = id.palette().color_bg_mid.to_hsla()[2];
            assert_eq!(
                l < 0.5,
                id.mode() == ThemeMode::Dark,
                "{id}: bg-mid lightness {l}"
            );
        }
    }

    fn rgb(hex: u32) -> Color {
        Color::from_rgb(hex)
    }
    fn layer(x: f32, y: f32, blur: f32, spread: f32, color: u32, inset: bool) -> ShadowLayer {
        ShadowLayer {
            x,
            y,
            blur,
            spread,
            color: Color(color),
            inset,
        }
    }

    // Values read by hand from globals.css; the comment says which rule decides each.
    #[test]
    fn nocturne_spot_checks() {
        let p = &NOCTURNE;
        assert_eq!(p.color_bg_darkest, rgb(0x0f0d13)); // @theme var(--t-chrome) → nocturne block
        assert_eq!(p.color_bg_tertiary, rgb(0x15121a)); // @theme var(--t-sidebar)
        assert_eq!(p.color_accent_soft, Color(0xffe4b817)); // rgba(255, 228, 184, 0.09): 22.95 → 0x17
        assert_eq!(p.color_live, rgb(0xd1362e)); // bare :root --t-live, every theme
        assert_eq!(p.color_danger, rgb(0xf0767e)); // @theme var(--t-error)
        assert_eq!(p.radius_xl, 18.0);
        assert_eq!(p.radius_avatar, LengthPct::Fraction(0.26));
        assert_eq!(p.font_tile, "Mona Sans"); // nocturne restates --t-font-tile over the :root default
        assert_eq!((p.t_tile_stretch, p.t_wordmark_stretch), (112.5, 125.0));
        assert_eq!(p.t_ls_body, 0.0); // restated 0em over :root's -0.01em
        assert_eq!(p.font_section, "Inter");
        assert_eq!(
            (p.font_meta, p.font_mono, p.font_display),
            ("Mona Sans", "JetBrains Mono", "Inter")
        );
        assert_eq!(
            p.shadow_float,
            &[
                layer(0.0, 12.0, 32.0, 0.0, 0x0000_0073, false),
                layer(0.0, 0.0, 0.0, 1.0, 0xe8e0_f814, false),
                layer(0.0, 1.0, 0.0, 0.0, 0xffff_ff0d, true),
            ]
        );
        assert_eq!(
            p.shadow_ring,
            &[layer(0.0, 0.0, 0.0, 2.0, 0xe8b9_6659, false)]
        );
        assert_eq!(
            (p.tile_tint, p.font_weight_emphasis, p.leading_body),
            (0.16, 600.0, 1.45)
        );
        assert_eq!(
            (p.ui_text_size_default, p.default_transition_duration),
            (14.5, 0.0)
        );
        assert_eq!(p.tracking_section, 0.12);
    }

    #[test]
    fn matinee_spot_checks() {
        let p = &MATINEE;
        assert_eq!(p.color_bg_mid, rgb(0xfaf9fc));
        assert_eq!(p.color_accent, rgb(0x6e3685));
        assert_eq!(p.color_accent_mid, Color(0x6e36_8526)); // rgba(110, 54, 133, 0.15)
        assert_eq!(p.color_on_accent, rgb(0xffffff));
        assert_eq!(p.color_text_muted, rgb(0x5c5866));
        assert_eq!(p.radius_xl, 18.0);
        assert_eq!(p.font_tile, "Mona Sans");
        assert_eq!(p.t_ls_body, 0.0);
        // 0.1 × 255 = 25.5 rounds up; an inset hairline as the second layer.
        assert_eq!(
            p.shadow_raised,
            &[
                layer(0.0, 1.0, 3.0, 0.0, 0x1c14_281a, false),
                layer(0.0, 0.0, 0.0, 1.0, 0x1c14_2812, true)
            ]
        );
        assert_eq!(p.tile_tint, 0.13);
        assert_eq!(p.radius_avatar, LengthPct::Fraction(0.26));
        assert_eq!(p.color_av_1, rgb(0xb75a59)); // :root --color-av-1 → matinee --t-av-1
    }

    #[test]
    fn graphite_spot_checks() {
        // `:root, :root[data-theme="graphite"]` + the graphite/graphite-light shared block.
        let p = &GRAPHITE;
        assert_eq!(p.color_accent, rgb(0x4f8cff));
        assert_eq!(p.color_accent_soft, Color(0x4f8c_ff21)); // rgba(79, 140, 255, 0.13)
                                                             // --t-shadow-ring: 0 0 0 2px var(--color-accent-soft) → @theme → --t-accent-soft.
        assert_eq!(
            p.shadow_ring,
            &[layer(0.0, 0.0, 0.0, 2.0, 0x4f8c_ff21, false)]
        );
        assert_eq!(
            p.shadow_raised,
            &[layer(0.0, 1.0, 2.0, 0.0, 0x0000_0066, false)]
        );
        // The shared block (0,2,0) beats the bare-:root default (0,1,0).
        assert_eq!(p.font_tile, "Mona Sans");
        assert_eq!(p.t_tile_stretch, 112.5);
        assert_eq!(p.t_ls_body, 0.0);
        assert_eq!(p.font_section, "Inter");
        assert_eq!(p.radius_xl, 20.0);
        assert_eq!(p.radius_avatar, LengthPct::Fraction(0.26));
        assert_eq!((p.font_display, p.font_meta), ("Inter", "Mona Sans"));
        assert_eq!(p.t_scrollbar, Color(0xffff_ff14));
    }

    #[test]
    fn graphite_light_spot_checks() {
        let p = &GRAPHITE_LIGHT;
        assert_eq!(p.color_bg_mid, rgb(0xfbfbfd));
        assert_eq!(
            p.shadow_ring,
            &[layer(0.0, 0.0, 0.0, 2.0, 0x2f6f_e038, false)]
        ); // its own, not accent-soft
        assert_eq!(
            p.shadow_glow,
            &[layer(0.0, 0.0, 0.0, 3.0, 0x2f6f_e02e, false)]
        );
        assert_eq!(p.font_tile, "Mona Sans");
        assert_eq!(p.t_ls_body, 0.0);
        assert_eq!(p.radius_xl, 20.0);
        assert_eq!(p.color_text_faint, rgb(0x5f6877));
        assert_eq!(p.tile_tint, 0.13);
        assert_eq!(p.color_name_4, rgb(0x256b42));
    }

    #[test]
    fn console_spot_checks() {
        let p = &CONSOLE;
        assert_eq!(p.color_bg_darkest, rgb(0x07080a));
        assert_eq!(
            (p.color_on_accent, p.color_on_error),
            (rgb(0x07120c), rgb(0x1a0b09))
        );
        assert_eq!(
            (p.radius_sm, p.radius_md, p.radius_lg, p.radius_xl),
            (3.0, 4.0, 4.0, 8.0)
        ); // flattened
        assert_eq!(p.radius_avatar, LengthPct::Px(4.0)); // var(--t-radius-md)
        assert_eq!(
            p.shadow_ring,
            &[layer(0.0, 0.0, 0.0, 2.0, 0x7ef0_a840, false)]
        ); // var(--color-accent-mid)
        assert_eq!(
            p.shadow_glow,
            &[layer(0.0, 0.0, 8.0, 0.0, 0x7ef0_a8ff, false)]
        );
        // The bare-:root typography defaults are console's.
        assert_eq!(p.font_tile, "JetBrains Mono");
        assert_eq!((p.t_tile_stretch, p.t_wordmark_stretch), (100.0, 100.0));
        assert_eq!(p.t_ls_body, -0.01);
        assert_eq!(p.font_section, "JetBrains Mono"); // var(--t-font-meta)
        assert_eq!((p.font_sans, p.font_display), ("Inter", "JetBrains Mono"));
        assert_eq!(
            (p.font_weight_emphasis, p.t_fs_body_n, p.tracking_section),
            (700.0, 13.0, 0.16)
        );
        assert_eq!(p.tile_tint, 0.22);
    }

    #[test]
    fn console_light_spot_checks() {
        let p = &CONSOLE_LIGHT;
        assert_eq!(p.color_bg_mid, rgb(0xf9faf9));
        assert_eq!(p.color_accent, rgb(0x0f7a45));
        assert_eq!(p.color_text_faint, rgb(0x616d67));
        assert_eq!(p.radius_avatar, LengthPct::Px(4.0));
        assert_eq!(
            p.shadow_glow,
            &[layer(0.0, 0.0, 0.0, 3.0, 0x0f7a_4529, false)]
        );
        assert_eq!(p.font_tile, "JetBrains Mono");
        assert_eq!(p.t_ls_body, -0.01);
        assert_eq!(p.tile_tint, 0.12);
        assert_eq!(p.font_section, "JetBrains Mono");
        assert_eq!(p.radius_xl, 8.0);
    }

    #[test]
    fn console_split_spot_checks() {
        // The canvas is console-light (shared block); the --tc-* ramp only reaches the chrome palette.
        assert_eq!(CONSOLE_SPLIT, CONSOLE_LIGHT);
        let c = chrome_palette(ThemeId::ConsoleSplit);
        assert_eq!(c.color_bg_darkest, rgb(0x07080a));
        assert_eq!(c.color_bg_dark, rgb(0x0c0e11));
        assert_eq!(c.color_text_primary, rgb(0xdfe4e2));
        assert_eq!(c.color_accent, rgb(0x7ef0a8));
        assert_eq!(c.color_accent_mid, Color(0x7ef0_a840));
        // Not re-pointed by .chrome-scope: inherited from the light root.
        assert_eq!(c.t_chrome, rgb(0xe7eae8));
        assert_eq!(c.t_scrollbar, Color(0x0a19_1229));
        assert_eq!(c.color_live, rgb(0xd1362e));
        assert_eq!(c.radius_sm, 3.0);
        // Shadows: --t-shadow-raised/float/ring/glow are swapped, modal is not; shadow-* follows --t-shadow-*.
        let dark_raised = [layer(0.0, 1.0, 2.0, 0.0, 0x0000_0080, false)];
        assert_eq!(
            (c.shadow_raised, c.t_shadow_raised),
            (&dark_raised[..], &dark_raised[..])
        );
        assert_eq!(
            c.shadow_ring,
            &[layer(0.0, 0.0, 0.0, 2.0, 0x7ef0_a840, false)]
        );
        assert_eq!(
            c.shadow_modal,
            &[
                layer(0.0, 24.0, 64.0, 0.0, 0x0a19_1238, false),
                layer(0.0, 0.0, 0.0, 1.0, 0x0a19_121c, false)
            ]
        );
        assert_eq!(c.shadow_modal, CONSOLE_LIGHT.shadow_modal);
    }

    #[test]
    fn scope_spot_checks() {
        // The image viewer carries .chrome-scope on the same element: console-split's stage is the dark chrome.
        let v = &scopes(ThemeId::ConsoleSplit).image_viewer;
        assert_eq!(v.viewer_stage, rgb(0x07080a));
        assert_eq!(
            (v.viewer_wash, v.viewer_wash_filter),
            (0.5, &[FilterFn::Blur(64.0), FilterFn::Saturate(1.35)][..])
        );
        let v = &scopes(ThemeId::ConsoleLight).image_viewer;
        assert_eq!(v.viewer_stage, rgb(0xe7eae8));
        assert_eq!(v.viewer_wash, 0.34);
        assert_eq!(
            v.viewer_wash_filter,
            &[
                FilterFn::Blur(64.0),
                FilterFn::Saturate(1.5),
                FilterFn::Brightness(1.2)
            ]
        );
        // The picture editor is portaled to <body>: console-split's veil is the light chrome.
        let e = &scopes(ThemeId::Nocturne).picture_editor;
        assert_eq!(
            (e.crop_veil, e.crop_edge),
            (Color(0x0f0d_13c7), Color(0xe8e7_eccc))
        );
        assert_eq!(
            scopes(ThemeId::ConsoleSplit).picture_editor.crop_veil,
            Color(0xe7ea_e8c7)
        );
        // The theme card of console-split: console chrome around the console-light canvas.
        let t = &scopes(ThemeId::ConsoleSplit).theme_preview;
        assert_eq!(
            (t.color_bg_darkest, t.color_bg_mid, t.color_bg_light),
            (rgb(0x07080a), rgb(0xf9faf9), rgb(0xf9faf9))
        );
        assert_eq!(scopes(ThemeId::Graphite).dsel_menu_above.dsel_dy, 4.0);
    }

    #[test]
    fn generated_color_mix_agrees_with_color_mix() {
        for id in ThemeId::ALL {
            let p = id.palette();
            let e = &id.scopes().picture_editor;
            assert_eq!(
                e.crop_veil,
                p.color_bg_darkest.mix(Color::TRANSPARENT, 0.78),
                "{id}"
            );
            assert_eq!(
                e.crop_corner,
                p.color_bg_darkest.mix(Color::TRANSPARENT, 0.42),
                "{id}"
            );
            assert_eq!(
                e.crop_edge,
                p.color_text_primary.mix(Color::TRANSPARENT, 0.80),
                "{id}"
            );
            assert_eq!(e.crop_grid, p.color_text_primary.scale_alpha(0.30), "{id}");
        }
    }

    #[test]
    fn scaled_at_the_default_and_the_extremes() {
        let close = |a: f32, b: f64| (a as f64 - b).abs() < 1e-5;
        // Mona Sans themes: the scale is 1 at the default 14.5 px.
        let s = scaled(&NOCTURNE, DEFAULT_TEXT_SIZE_PX, DEFAULT_ROW_SCALE);
        assert_eq!(
            (s.ui_text_scale, s.text_title, s.text_body, s.text_section),
            (1.0, 15.0, 14.5, 11.25)
        );
        assert_eq!(
            (s.text_meta, s.text_micro, s.list_row_avatar_scale),
            (11.5, 11.0, 1.0)
        );
        assert!(close(s.text_reply, 13.05));
        assert_eq!(s.text_reply_line_height, 18.0); // round(up, 13.05 × 1.375 = 17.94375, 1px)
        let s = scaled(&NOCTURNE, 11.0, 0.85);
        assert!(
            close(s.text_title, 15.0 * 11.0 / 14.5) && close(s.text_section, 11.25 * 11.0 / 14.5)
        );
        assert!(close(s.text_body, 11.0) && close(s.text_reply, 9.9));
        assert_eq!(s.text_reply_line_height, 14.0); // 13.6125 → 14
        assert_eq!(s.list_row_avatar_scale, 0.85);
        let s = scaled(&MATINEE, 17.0, 1.3);
        assert!(
            close(s.text_body, 17.0)
                && close(s.text_reply, 15.3)
                && close(s.text_title, 15.0 * 17.0 / 14.5)
        );
        assert_eq!(s.text_reply_line_height, 22.0); // 21.0375 → 22
                                                    // Console: natural body 13, so the default 14.5 already scales by 14.5 / 13.
        let s = scaled(&CONSOLE, 14.5, 1.0);
        assert!(
            close(s.ui_text_scale, 14.5 / 13.0)
                && close(s.text_body, 14.5)
                && close(s.text_title, 14.5)
        );
        assert!(
            close(s.text_section, 9.5 * 14.5 / 13.0) && close(s.text_sender, 12.5 * 14.5 / 13.0)
        );
        assert_eq!(s.text_reply_line_height, 18.0);
        let s = scaled(&CONSOLE_SPLIT, 11.0, 1.0);
        assert!(close(s.text_member, 12.0 * 11.0 / 13.0) && close(s.text_reply, 9.9));
        assert_eq!(s.text_reply_line_height, 14.0);
        let s = scaled(&CONSOLE_LIGHT, 17.0, 1.0);
        assert!(close(s.text_micro, 9.5 * 17.0 / 13.0));
        assert_eq!(s.text_reply_line_height, 22.0);
        // 16.5 px: 14.85 × 1.375 = 20.41875 → 21 in every theme.
        for id in ThemeId::ALL {
            assert_eq!(
                scaled(id.palette(), 16.5, 1.0).text_reply_line_height,
                21.0,
                "{id}"
            );
        }
    }

    #[test]
    fn css_round_semantics() {
        use RoundingStrategy::*;
        assert_eq!(css_round(Up, 17.94375, 1.0), 18.0);
        assert_eq!(css_round(Up, 18.0, 1.0), 18.0);
        assert_eq!(css_round(Down, 17.9, 1.0), 17.0);
        assert_eq!(css_round(Nearest, 2.5, 1.0), 3.0);
        assert_eq!(css_round(Nearest, -2.5, 1.0), -2.0); // ties towards +∞, unlike f64::round
        assert_eq!(css_round(Nearest, 2.4, 1.0), 2.0);
        assert_eq!(css_round(ToZero, -2.7, 1.0), -2.0);
        assert_eq!(css_round(Nearest, 7.0, 4.0), 8.0);
        assert!(css_round(Up, 1.0, 0.0).is_nan());
    }

    #[test]
    fn settings_clamps_follow_ui_store() {
        assert_eq!(clamp_text_size_px(0.0), 14.5);
        assert_eq!(clamp_text_size_px(f32::NAN), 14.5);
        assert_eq!(clamp_text_size_px(14.2), 14.0);
        assert_eq!(clamp_text_size_px(14.25), 14.5); // Math.round(28.5) = 29
        assert_eq!(clamp_text_size_px(30.0), 17.0);
        assert_eq!(clamp_text_size_px(3.0), 11.0);
        assert_eq!(clamp_row_scale(1.050_000_1), 1.05);
        assert_eq!(clamp_row_scale(2.0), 1.3);
        assert_eq!(clamp_row_scale(0.1), 0.85);
        assert_eq!(clamp_row_scale(f32::INFINITY), 1.0);
        assert_eq!(
            (TEXT_SIZE_MIN_PX, TEXT_SIZE_MAX_PX, TEXT_SIZE_STEP_PX),
            (11.0, 17.0, 0.5)
        );
        assert_eq!(
            (
                ROW_SCALE_MIN,
                ROW_SCALE_MAX,
                ROW_SCALE_STEP,
                DEFAULT_ROW_SCALE
            ),
            (0.85, 1.3, 0.05, 1.0)
        );
        // The CSS fallback for an unset --ui-text-size-n is uiStore's default.
        assert!(THEMES
            .iter()
            .all(|t| t.2.ui_text_size_default == DEFAULT_TEXT_SIZE_PX));
    }

    #[test]
    fn color_conversions() {
        let c = Color(0x1122_3344);
        assert_eq!((c.r(), c.g(), c.b(), c.a()), (0x11, 0x22, 0x33, 0x44));
        assert_eq!(Color::from_rgba8(0x11, 0x22, 0x33, 0x44), c);
        assert_eq!(Color::from_rgb(0xe8b966), Color(0xe8b9_66ff));
        assert_eq!(c.with_alpha(0.5), Color(0x1122_3380)); // 127.5 → 128
        assert_eq!(c.with_alpha(2.0), Color(0x1122_33ff));
        assert_eq!(c.with_alpha(f32::NAN), Color(0x1122_3300));
        assert_eq!(Color::WHITE.scale_alpha(0.5), Color(0xffff_ff80));
        assert_eq!(
            c.to_rgba_f32(),
            [17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 68.0 / 255.0]
        );
        assert_eq!(rgb(0xff0000).to_hsla(), [0.0, 1.0, 0.5, 1.0]);
        let [h, s, l, a] = rgb(0x00ff00).to_hsla();
        assert!((h - 1.0 / 3.0).abs() < 1e-6 && s == 1.0 && l == 0.5 && a == 1.0);
        assert_eq!(rgb(0x808080).to_hsla(), [0.0, 0.0, 128.0 / 255.0, 1.0]);
        let [h, s, l, _] = rgb(0xe8b966).to_hsla(); // brass: 38.3°, 73.9 %, 65.5 %
        assert!(
            (h * 360.0 - 38.3077).abs() < 1e-3
                && (s - 0.738636).abs() < 1e-5
                && (l - 0.654902).abs() < 1e-5
        );
        // color-mix(in srgb, …)
        assert_eq!(rgb(0xff0000).mix(rgb(0x0000ff), 0.5), Color(0x8000_80ff));
        assert_eq!(rgb(0xbf5e5e).mix(rgb(0x232029), 0.16), Color(0x3c2a_31ff)); // a nocturne voice-tile tint
        assert_eq!(
            Color::TRANSPARENT.mix(Color::TRANSPARENT, 0.3),
            Color::TRANSPARENT
        );
        assert_eq!(format!("{:?}", c), "#11223344");
    }
}
