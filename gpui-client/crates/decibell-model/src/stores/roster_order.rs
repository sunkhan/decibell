//! Roster order ("Roster order" in `proto/messages.proto`), mirrored exactly from the
//! community server: pages arrive in this order, so the sidebar sorts by it too and a
//! page only ever extends the list at its end. The fold is `search.cpp`'s `search_fold`;
//! keep the two in step (`features/members/rosterOrder.ts` is the renderer's copy).

use std::cmp::Ordering;

use gpui::SharedString;

use crate::ids::Uid;

/// Members per roster page: the first holds every online member of all but big
/// communities; later ones load as the sidebar scrolls.
pub const ROSTER_FIRST_PAGE: i32 = 500;
pub const ROSTER_PAGE: i32 = 200;

/// Folded code point of U+00C0..U+017F (`search.cpp` `kLatinFold`): lower case, accents
/// removed, the Turkish dotless ı (U+0131) as i.
#[rustfmt::skip]
const LATIN_FOLD: [u16; 0x180 - 0xC0] = [
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00e6, 0x0063, // U+00C0
    0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069, // U+00C8
    0x00f0, 0x006e, 0x006f, 0x006f, 0x006f, 0x006f, 0x006f, 0x00d7, // U+00D0
    0x00f8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00fe, 0x00df, // U+00D8
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00e6, 0x0063, // U+00E0
    0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069, // U+00E8
    0x00f0, 0x006e, 0x006f, 0x006f, 0x006f, 0x006f, 0x006f, 0x00f7, // U+00F0
    0x00f8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00fe, 0x0079, // U+00F8
    0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0063, 0x0063, // U+0100
    0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0064, 0x0064, // U+0108
    0x0111, 0x0111, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065, // U+0110
    0x0065, 0x0065, 0x0065, 0x0065, 0x0067, 0x0067, 0x0067, 0x0067, // U+0118
    0x0067, 0x0067, 0x0067, 0x0067, 0x0068, 0x0068, 0x0127, 0x0127, // U+0120
    0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, // U+0128
    0x0069, 0x0069, 0x0133, 0x0133, 0x006a, 0x006a, 0x006b, 0x006b, // U+0130
    0x0138, 0x006c, 0x006c, 0x006c, 0x006c, 0x006c, 0x006c, 0x0140, // U+0138
    0x0140, 0x0142, 0x0142, 0x006e, 0x006e, 0x006e, 0x006e, 0x006e, // U+0140
    0x006e, 0x0149, 0x014b, 0x014b, 0x006f, 0x006f, 0x006f, 0x006f, // U+0148
    0x006f, 0x006f, 0x0153, 0x0153, 0x0072, 0x0072, 0x0072, 0x0072, // U+0150
    0x0072, 0x0072, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073, // U+0158
    0x0073, 0x0073, 0x0074, 0x0074, 0x0074, 0x0074, 0x0167, 0x0167, // U+0160
    0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, // U+0168
    0x0075, 0x0075, 0x0075, 0x0075, 0x0077, 0x0077, 0x0079, 0x0079, // U+0170
    0x0079, 0x007a, 0x007a, 0x007a, 0x007a, 0x007a, 0x007a, 0x017f, // U+0178
];

fn fold_code_point(c: u32) -> u32 {
    if c < 0x80 {
        return if (0x41..=0x5a).contains(&c) { c + 32 } else { c };
    }
    if (0xc0..0x180).contains(&c) {
        return LATIN_FOLD[(c - 0xc0) as usize] as u32;
    }
    if (0x391..=0x3a9).contains(&c) && c != 0x3a2 {
        return c + 0x20; // Greek capitals
    }
    if (0x410..=0x42f).contains(&c) {
        return c + 0x20; // Cyrillic А..Я
    }
    if (0x400..=0x40f).contains(&c) {
        return c + 0x50; // Cyrillic Ѐ..Џ
    }
    c
}

/// `search_fold`: lower case, accents removed (combining marks dropped).
pub fn fold_name(s: &str) -> String {
    s.chars()
        .filter(|ch| !(0x300..=0x36f).contains(&(*ch as u32)))
        .filter_map(|ch| char::from_u32(fold_code_point(ch as u32)))
        .collect()
}

/// A member's place in roster order: the folded display name, the display name, the uid.
/// Strings compare by code point, which is the server's UTF-8 byte order (and what
/// `str`'s `Ord` does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterKey {
    pub fold: String,
    pub name: SharedString,
    pub uid: Uid,
}

impl RosterKey {
    /// From the community's own names (nickname, else the username it holds), never the
    /// users store's: the order must be the server's, even where central knows a newer
    /// name (a rename the community hasn't seen yet only changes the label).
    pub fn new(nickname: &SharedString, username: &SharedString, uid: Uid) -> RosterKey {
        let name = if nickname.is_empty() { username } else { nickname };
        RosterKey {
            fold: fold_name(name),
            name: name.clone(),
            uid,
        }
    }
}

impl Ord for RosterKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.fold
            .cmp(&other.fold)
            .then_with(|| self.name.as_ref().cmp(other.name.as_ref()))
            .then_with(|| self.uid.cmp(&other.uid))
    }
}

impl PartialOrd for RosterKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, uid: Uid) -> RosterKey {
        RosterKey::new(&"".into(), &name.to_string().into(), uid)
    }

    #[test]
    fn fold_lowercases_and_strips_accents() {
        assert_eq!(fold_name("Alice"), "alice");
        assert_eq!(fold_name("ÉMILE Zola"), "emile zola");
        assert_eq!(fold_name("Günhan"), "gunhan");
        // Turkish: dotted İ and dotless ı both fold to i.
        assert_eq!(fold_name("Işık İnci"), "isik inci");
        // Combining marks are dropped.
        assert_eq!(fold_name("e\u{301}clair"), "eclair");
        assert_eq!(fold_name("ΑΘΗΝΑ"), "αθηνα");
        assert_eq!(fold_name("Москва"), "москва");
        assert_eq!(fold_name("Ђорђе"), "ђорђе");
        // Untouched: digits, emoji, CJK, letters the table keeps (ß, æ).
        assert_eq!(fold_name("Bobby 🛠 42"), "bobby 🛠 42");
        assert_eq!(fold_name("Straße Æon"), "straße æon");
    }

    #[test]
    fn order_is_fold_then_name_then_uid() {
        let mut keys = [key("bob", 2), key("Zed", 9), key("alice", 1), key("Bob", 3), key("bob", 1), key("Émile", 4), key("emile", 5)];
        keys.sort();
        let order: Vec<(&str, Uid)> = keys.iter().map(|k| (&*k.name, k.uid)).collect();
        assert_eq!(
            order,
            [("alice", 1), ("Bob", 3), ("bob", 1), ("bob", 2), ("emile", 5), ("Émile", 4), ("Zed", 9)]
        );
    }

    #[test]
    fn code_point_order_holds_past_the_bmp() {
        // U+FFFD sorts before U+1F600 by code point (UTF-16 units would say otherwise).
        assert!(key("\u{fffd}", 1) < key("\u{1f600}", 1));
        assert!(key("a", 1) < key("a\u{1f600}", 1));
    }

    #[test]
    fn the_nickname_is_the_display_name() {
        let k = RosterKey::new(&"Bobby 🛠".into(), &"bob".into(), 2);
        assert_eq!(&*k.name, "Bobby 🛠");
        assert_eq!(k.fold, "bobby 🛠");
    }
}
