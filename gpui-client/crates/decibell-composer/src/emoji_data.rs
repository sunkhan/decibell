//! The picker's emoji (`@emoji-mart/data`, compiled in by `build.rs`): static data, nothing parsed
//! or allocated until a search runs. See `EmojiPicker.tsx`.

/// One emoji of the dataset, as the renderer reads it.
#[derive(Debug)]
pub struct Emoji {
    /// The shortcode id (`grinning`): what the preview bar shows as `:id:`.
    pub id: &'static str,
    /// The tooltip (`title`).
    pub name: &'static str,
    pub name_lower: &'static str,
    pub keywords_lower: &'static [&'static str],
    /// `skins[0].native`: what is inserted (there is no skin-tone chooser in the renderer).
    pub native: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/emoji_data.rs"));

/// `CATEGORY_LABELS`.
pub fn category_label(id: &str) -> &'static str {
    match id {
        "frequent" => "Frequently used",
        "people" => "Smileys & People",
        "nature" => "Animals & Nature",
        "foods" => "Food & Drink",
        "activity" => "Activity",
        "places" => "Travel & Places",
        "objects" => "Objects",
        "symbols" => "Symbols",
        "flags" => "Flags",
        _ => "",
    }
}

/// `CATEGORY_ICONS` (drawn as Twemoji).
pub fn category_icon(id: &str) -> &'static str {
    match id {
        "frequent" => "🕘",
        "people" => "😀",
        "nature" => "🐶",
        "foods" => "🍔",
        "activity" => "⚽",
        "places" => "🚗",
        "objects" => "💡",
        "symbols" => "❤️",
        "flags" => "🏁",
        _ => "❓",
    }
}

/// `RECENT_MAX`: the "Frequently used" category's length.
pub const RECENT_MAX: usize = 24;
/// The search's cap.
pub const SEARCH_MAX: usize = 120;

/// The index of an emoji id (`EMOJIS[id]`).
pub fn index_of(id: &str) -> Option<u16> {
    BY_ID
        .binary_search_by(|index| EMOJI[*index as usize].id.cmp(id))
        .ok()
        .map(|at| BY_ID[at])
}

pub fn get(index: u16) -> &'static Emoji {
    &EMOJI[index as usize]
}

/// `searchResults`: the trimmed, lower-cased query as a substring of the id, the name or a
/// keyword, in the dataset's order, at most 120. Empty for an empty query.
pub fn search(query: &str) -> Vec<u16> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, emoji) in EMOJI.iter().enumerate() {
        if emoji.id.contains(&query) || emoji.name_lower.contains(&query) || emoji.keywords_lower.iter().any(|k| k.contains(&query)) {
            out.push(index as u16);
            if out.len() >= SEARCH_MAX {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dataset_keeps_the_renderers_order_and_search() {
        if EMOJI.is_empty() {
            return; // assets not fetched
        }
        assert_eq!(EMOJI.len(), 1870);
        assert_eq!((EMOJI[0].id, EMOJI[1].id), ("100", "1234"));
        assert_eq!(CATEGORIES.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["people", "nature", "foods", "activity", "places", "objects", "symbols", "flags"]);
        assert_eq!(get(CATEGORIES[0].1[0]).id, "grinning");
        assert_eq!(index_of("grinning").map(|i| get(i).native), Some("😀"));
        assert_eq!(index_of("not-an-emoji"), None);
        // "smile": ids, names and keywords, in key order, as Object.entries walks them.
        let found: Vec<&str> = search("  Smile ").iter().map(|i| get(*i).id).collect();
        assert!(found.contains(&"smile") && found.contains(&"smiley"), "{found:?}");
        assert!(search("a").len() == SEARCH_MAX);
        assert!(search("   ").is_empty());
        // Keywords with capitals are compared lower-cased (`k.toLowerCase()`).
        assert!(search("xd").iter().any(|i| get(*i).keywords_lower.contains(&"xd")));
    }
}
