//! The assets compiled into the binary, as GPUI's [`AssetSource`]: every SVG under
//! `gpui-client/assets/icons` (the table is written by `build.rs`), borrowed from the
//! executable's read-only data, never copied.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// Pass to `Application::with_assets`.
pub struct Assets;

impl Assets {
    /// The embedded file at `path` (`icons/close.svg`).
    pub(crate) fn load_icon(&self, path: &str) -> Option<&'static [u8]> {
        let index = ICONS.binary_search_by_key(&path, |(name, _)| name).ok()?;
        Some(ICONS[index].1)
    }
}

impl AssetSource for Assets {
    /// `icons/lucide/settings.svg`, or with a stroke width other than the file's own:
    /// `icons/lucide/settings.svg#stroke-width=1.5` (what [`crate::Icon::stroke_width`] asks
    /// for; GPUI caches the rasterised icon by this path and its size).
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let (file, stroke_width) = match path.split_once("#stroke-width=") {
            Some((file, width)) => (file, Some(width)),
            None => (path, None),
        };
        let Some(bytes) = self.load_icon(file) else {
            return Ok(None);
        };
        Ok(Some(match stroke_width {
            None => Cow::Borrowed(bytes),
            Some(width) => Cow::Owned(restroke(std::str::from_utf8(bytes)?, width).into_bytes()),
        }))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::new_static(name))
            .collect())
    }
}

/// The SVG with its stroke width replaced: the value of every `stroke-width` attribute that has
/// the file's main width (the first one written, on the root `<svg>` in the renderer's glyphs
/// and lucide's), so details drawn at another width keep theirs.
fn restroke(svg: &str, width: &str) -> String {
    const ATTRIBUTE: &str = "stroke-width=\"";
    let Some(start) = svg.find(ATTRIBUTE).map(|at| at + ATTRIBUTE.len()) else {
        return svg.to_owned();
    };
    let Some(end) = svg[start..].find('"').map(|len| start + len) else {
        return svg.to_owned();
    };
    let main = &svg[start - ATTRIBUTE.len()..=end];
    svg.replace(main, &format!("{ATTRIBUTE}{width}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_embedded_sorted_and_restroked() {
        assert!(ICONS.is_sorted_by_key(|(name, _)| name));
        let plain = Assets.load("icons/lucide/settings.svg").unwrap().unwrap();
        assert!(matches!(plain, Cow::Borrowed(_)));
        let thin = Assets
            .load("icons/lucide/settings.svg#stroke-width=1.5")
            .unwrap()
            .unwrap();
        let thin = std::str::from_utf8(&thin).unwrap();
        assert!(thin.contains("stroke-width=\"1.5\"") && !thin.contains("stroke-width=\"2\""));
        assert!(Assets.load("icons/window/close.svg").unwrap().is_some());
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert_eq!(Assets.list("icons/window/").unwrap().len(), 4);
    }

    #[test]
    fn restroke_keeps_other_widths() {
        let svg = r#"<svg stroke-width="2.5"><path stroke-width="1"/><path/></svg>"#;
        assert_eq!(
            restroke(svg, "3"),
            r#"<svg stroke-width="3"><path stroke-width="1"/><path/></svg>"#
        );
    }
}
