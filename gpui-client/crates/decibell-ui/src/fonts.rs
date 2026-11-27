//! The bundled fonts.
//!
//! GPUI at our pin cannot set variable-font axes and has no font width, so the client embeds
//! static faces cut from the renderer's fonts by `gpui-client/tools/gen-fonts.sh`, one family
//! per width, under names that cannot collide with a font installed on the machine:
//!
//! | Family | Is | Weights |
//! |---|---|---|
//! | `Decibell Inter` | Inter at optical size 14 | 400 500 600 700 |
//! | `Decibell Sans` | Mona Sans, width 100 | 400 500 600 700 |
//! | `Decibell Sans Plus` | Mona Sans, width 112.5 (server tiles) | 400 500 600 700 |
//! | `Decibell Sans Max` | Mona Sans, width 125 (the wordmark) | 600 |
//! | `Decibell Mono` | JetBrains Mono | 400 500 600 700 |
//!
//! View code never names a family: it asks the theme (`cx.theme().font(FontRole::Meta)`).
//! Any other weight snaps to the nearest face, as in CSS. There is no italic face (the
//! renderer has none either and lets Chromium slant the upright one): asking for italic gives
//! a slanted twin of the upright face (GPUI patch 0010, the skew Chromium uses), with its own
//! `FontId` and the same advances.

use std::borrow::Cow;

use anyhow::Result;
use gpui::App;

mod generated {
    include!("fonts_generated.rs");
}
pub use generated::FACES;

/// One embedded face. The measurements of its `m` are there to check font matching: a text
/// system that resolves (`family`, `weight`) to a face with another `m` picked the wrong one.
pub struct Face {
    pub family: &'static str,
    pub weight: u16,
    pub file: &'static str,
    pub units_per_em: u16,
    /// The advance of `m` in font units. Differs between the weights of the proportional
    /// families; the same for every weight of the monospace one.
    pub advance_m: u16,
    /// The width of the outline of `m` in font units. Differs between all faces of a family.
    pub ink_width_m: u16,
    pub bytes: &'static [u8],
}

impl Face {
    /// Whether no other face of the family has the same `advance_m`.
    pub fn advance_identifies(&self) -> bool {
        !FACES.iter().any(|other| {
            other.family == self.family
                && other.weight != self.weight
                && other.advance_m == self.advance_m
        })
    }
}

/// Hands every face to GPUI's text system, borrowed from the binary (no copy on Linux, where
/// they stay file-backed pages; DirectWrite copies them). Call once, before the first window.
pub fn register(cx: &App) -> Result<()> {
    cx.text_system()
        .add_fonts(FACES.iter().map(|face| Cow::Borrowed(face.bytes)).collect())
}

/// The bundled family for a theme token's family (`"Inter"`, `"Mona Sans"`,
/// `"JetBrains Mono"`) at a CSS `font-stretch` percentage. Only Mona Sans has widths.
pub fn family(token_family: &str, stretch: f32) -> &'static str {
    match token_family {
        "Mona Sans" if stretch >= 118.75 => "Decibell Sans Max",
        "Mona Sans" if stretch >= 106.25 => "Decibell Sans Plus",
        "Mona Sans" => "Decibell Sans",
        "JetBrains Mono" => "Decibell Mono",
        other => {
            debug_assert_eq!(other, "Inter", "a theme names a family that is not bundled");
            "Decibell Inter"
        }
    }
}

/// Font matching through GPUI's real Linux text system (cosmic-text + font-kit's matcher) with
/// nothing but the bundled faces loaded. The same check runs against the platform's text
/// system, system fonts included, in the app's `boot` self-test (that is the one that covers
/// DirectWrite).
#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod tests {
    use super::*;
    use gpui::{
        Font, FontFeatures, FontId, FontStyle, FontWeight, PlatformTextSystem, RenderGlyphParams,
        px,
    };

    fn text_system() -> gpui_wgpu::CosmicTextSystem {
        let text_system = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter");
        text_system
            .add_fonts(FACES.iter().map(|face| Cow::Borrowed(face.bytes)).collect())
            .unwrap();
        text_system
    }

    fn resolve(
        text_system: &impl PlatformTextSystem,
        family: &str,
        weight: f32,
        style: FontStyle,
    ) -> FontId {
        text_system
            .font_id(&Font {
                family: family.to_string().into(),
                features: FontFeatures::default(),
                fallbacks: None,
                weight: FontWeight(weight),
                style,
            })
            .unwrap_or_else(|error| panic!("{family} {weight}: {error}"))
    }

    #[test]
    fn every_face_resolves_to_itself() {
        let text_system = text_system();
        let mut ids = Vec::new();
        for face in FACES.iter() {
            let id = resolve(
                &text_system,
                face.family,
                face.weight as f32,
                FontStyle::Normal,
            );
            assert!(
                !ids.contains(&id),
                "{} {} shares a face",
                face.family,
                face.weight
            );
            ids.push(id);
            let glyph = text_system.glyph_for_char(id, 'm').unwrap();
            assert_eq!(
                text_system.advance(id, glyph).unwrap().width,
                face.advance_m as f32,
                "{} {} resolved to another face",
                face.family,
                face.weight
            );

            // The outline itself, which also tells the monospace weights apart: the ink of
            // `m` rasterised at half an em per pixel must be nearer this face's than any
            // other of the family.
            let params = RenderGlyphParams {
                font_id: id,
                glyph_id: glyph,
                font_size: px(face.units_per_em as f32 / 2.),
                subpixel_variant: Default::default(),
                scale_factor: 1.,
                is_emoji: false,
                subpixel_rendering: false,
                dilation: 0,
            };
            let bounds = text_system.glyph_raster_bounds(&params).unwrap();
            text_system.rasterize_glyph(&params, bounds).unwrap();
            let ink = bounds.size.width.0 as f32 * 2.;
            let nearest = FACES
                .iter()
                .filter(|other| other.family == face.family)
                .min_by_key(|other| (other.ink_width_m as f32 - ink).abs() as u32)
                .unwrap();
            assert_eq!(
                nearest.weight, face.weight,
                "{} {}: the rasterised m is {ink} units wide, the face's is {}",
                face.family, face.weight, face.ink_width_m
            );
        }
    }

    #[test]
    fn the_measurements_tell_the_faces_of_a_family_apart() {
        for (index, face) in FACES.iter().enumerate() {
            for other in &FACES[index + 1..] {
                if face.family == other.family {
                    assert!(face.ink_width_m.abs_diff(other.ink_width_m) >= 4);
                    assert_eq!(
                        face.advance_m == other.advance_m,
                        face.family == "Decibell Mono"
                    );
                }
            }
        }
    }

    #[test]
    fn other_weights_snap_to_the_nearest_face_and_italic_is_a_slanted_twin() {
        let text_system = text_system();
        let regular = resolve(&text_system, "Decibell Inter", 400., FontStyle::Normal);
        let bold = resolve(&text_system, "Decibell Inter", 700., FontStyle::Normal);
        assert_eq!(
            resolve(&text_system, "Decibell Inter", 300., FontStyle::Normal),
            regular
        );
        assert_eq!(
            resolve(&text_system, "Decibell Inter", 900., FontStyle::Normal),
            bold
        );
        // No italic face is bundled: italic is a slanted twin of the upright face (patch 0010),
        // a different font id so the glyph cache keeps the two apart.
        assert_ne!(
            resolve(&text_system, "Decibell Inter", 400., FontStyle::Italic),
            regular
        );
        // The wordmark family has one weight; every request lands on it.
        let wordmark = resolve(
            &text_system,
            "Decibell Sans Max",
            600.,
            FontStyle::Normal,
        );
        assert_eq!(
            resolve(
                &text_system,
                "Decibell Sans Max",
                400.,
                FontStyle::Normal
            ),
            wordmark
        );
    }

    #[test]
    fn theme_families_map_to_bundled_ones() {
        assert_eq!(family("Inter", 100.), "Decibell Inter");
        assert_eq!(family("Mona Sans", 100.), "Decibell Sans");
        assert_eq!(family("Mona Sans", 112.5), "Decibell Sans Plus");
        assert_eq!(family("Mona Sans", 125.), "Decibell Sans Max");
        assert_eq!(family("JetBrains Mono", 112.5), "Decibell Mono");
    }
}
