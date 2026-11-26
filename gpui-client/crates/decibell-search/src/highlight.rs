//! Which parts of a result's text are marked (`features/search/highlight.tsx`): the words the
//! index matched — whole words, case- and accent-insensitive, and words starting with a 3+ letter
//! search word — or, with "Inside words", every place a fragment occurs, inside words too. Text
//! and words are folded alike (`decibell_model::chat::search::fold`: lower case, accents off, the
//! dotless ı as i), and marks map back to the original characters.
//!
//! A [`Highlighter`] gives byte ranges of the text it is handed; the result row applies it to each
//! text run of the body separately, as the renderer applies it to each text node.

use std::collections::HashSet;
use std::ops::Range;

use decibell_model::chat::search::{SearchQuery, SearchWord, fold, search_fragments, search_words};
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory as _};

/// What a search marks: built once per query (`makeHighlighter` / `makeFragmentHighlighter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Highlighter {
    Words { exact: HashSet<String>, prefixes: Vec<String> },
    Fragments(Vec<String>),
}

impl Highlighter {
    /// The highlighter of a query, `None` when there is nothing to mark (the renderer's
    /// `undefined`: no work at all).
    pub fn for_query(query: &SearchQuery) -> Option<Highlighter> {
        if query.matches_inside() { Self::fragments(search_fragments(&query.text)) } else { Self::words(&search_words(&query.text)) }
    }

    /// `makeHighlighter`.
    pub fn words(words: &[SearchWord]) -> Option<Highlighter> {
        if words.is_empty() {
            return None;
        }
        let exact = words.iter().filter(|w| !w.prefix).map(|w| w.folded.clone()).collect();
        let prefixes = words.iter().filter(|w| w.prefix).map(|w| w.folded.clone()).collect();
        Some(Highlighter::Words { exact, prefixes })
    }

    /// `makeFragmentHighlighter`.
    pub fn fragments(fragments: Vec<String>) -> Option<Highlighter> {
        let fragments: Vec<String> = fragments.into_iter().filter(|f| !f.is_empty()).collect();
        (!fragments.is_empty()).then_some(Highlighter::Fragments(fragments))
    }

    /// The marked byte ranges of `text`, in order, never overlapping.
    pub fn marks(&self, text: &str) -> Vec<Range<usize>> {
        match self {
            Highlighter::Words { exact, prefixes } => word_marks(text, exact, prefixes),
            Highlighter::Fragments(fragments) => fragment_marks(text, fragments),
        }
    }
}

fn group(c: char) -> GeneralCategoryGroup {
    c.general_category_group()
}

/// `/[\p{L}\p{N}][\p{L}\p{N}\p{M}]*/gu`: letters and digits plus their combining marks, so a
/// decomposed "café" stays one word.
fn runs(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let start = loop {
            let (at, c) = chars.next()?;
            if matches!(group(c), GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number) {
                break at;
            }
        };
        let mut end = text[start..].chars().next().map_or(start, |c| start + c.len_utf8());
        while let Some(&(at, c)) = chars.peek() {
            if !matches!(group(c), GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number | GeneralCategoryGroup::Mark) {
                break;
            }
            end = at + c.len_utf8();
            chars.next();
        }
        Some(start..end)
    })
}

fn word_marks(text: &str, exact: &HashSet<String>, prefixes: &[String]) -> Vec<Range<usize>> {
    runs(text)
        .filter(|run| {
            let folded = fold(&text[run.clone()]);
            exact.contains(&folded) || prefixes.iter().any(|p| folded.starts_with(p.as_str()))
        })
        .collect()
}

fn fragment_marks(text: &str, fragments: &[String]) -> Vec<Range<usize>> {
    // folded[i] came from text[at[i]]; at[folded.len()] = text.len().
    let mut folded = String::with_capacity(text.len());
    let mut at: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let mut buf = [0u8; 4];
    for (i, c) in text.char_indices() {
        let f = fold(c.encode_utf8(&mut buf));
        at.extend(std::iter::repeat_n(i, f.len()));
        folded.push_str(&f);
    }
    at.push(text.len());
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for fragment in fragments {
        let mut from = 0;
        while let Some(found) = folded[from..].find(fragment.as_str()) {
            let p = from + found;
            spans.push((at[p], at[p + fragment.len()]));
            // `indexOf(frag, p + 1)`: the next start, overlapping ones too.
            from = p + folded[p..].chars().next().map_or(1, char::len_utf8);
        }
    }
    spans.sort_by_key(|span| span.0);
    let mut out = Vec::new();
    let mut last = 0;
    for (s0, e0) in spans {
        let s = s0.max(last);
        if e0 <= s {
            continue;
        }
        out.push(s..e0);
        last = e0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(h: &Highlighter, text: &str) -> Vec<String> {
        h.marks(text).into_iter().map(|r| text[r].to_string()).collect()
    }

    #[test]
    fn words_whole_and_by_prefix() {
        let h = Highlighter::words(&search_words("wayland gg")).unwrap();
        assert_eq!(marked(&h, "Wayland? waylander gg ggs, GG!"), vec!["Wayland", "waylander", "gg", "GG"]);
        let h = Highlighter::words(&search_words("cafe")).unwrap();
        assert_eq!(marked(&h, "Café, cafés and CAFE\u{301}"), vec!["Café", "cafés", "CAFE\u{301}"]);
        let h = Highlighter::words(&search_words("isik")).unwrap();
        assert_eq!(marked(&h, "IŞIK ışık isik"), vec!["IŞIK", "ışık", "isik"], "ı and i match each other");
        assert!(Highlighter::words(&search_words("-only")).is_none());
    }

    #[test]
    fn fragments_inside_words() {
        let h = Highlighter::fragments(search_fragments("ayl")).unwrap();
        assert_eq!(marked(&h, "Wayland waylander"), vec!["ayl", "ayl"]);
        let h = Highlighter::fragments(search_fragments("aa")).unwrap();
        assert_eq!(h.marks("aaaa"), vec![0..2, 2..3, 3..4], "overlapping finds: each one past the last, as the renderer splits them");
        let h = Highlighter::fragments(search_fragments("ıs")).unwrap();
        assert_eq!(marked(&h, "IŞIK"), vec!["IŞ"]);
        let h = Highlighter::fragments(search_fragments("e")).unwrap();
        assert_eq!(marked(&h, "cafe\u{301}!"), vec!["e\u{301}"], "a combining mark folds to nothing and goes with its letter");
    }
}
