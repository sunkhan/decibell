//! Port of KaTeX `src/symbols.ts` (and `src/atoms.ts`): the table of single-character
//! symbols and no-argument commands per mode. The data itself is generated into
//! `data/symbols_data.rs` (sorted by key, binary-searched).

use crate::data::symbols_data;
use crate::parse_node::Mode;

/// `SymbolFont`: which font family a symbol is drawn from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolFont {
    Main,
    Ams,
}

/// `Group` (atoms.ts): the parse-node group a symbol produces. The first seven are the
/// `Atom` families (`isAtom`), the rest the non-atom symbol node types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Bin,
    Close,
    Inner,
    Open,
    Punct,
    Rel,
    AccentToken,
    MathOrd,
    OpToken,
    Spacing,
    TextOrd,
}

impl Group {
    /// `isAtom(group)`.
    pub fn is_atom(self) -> bool {
        matches!(
            self,
            Group::Bin | Group::Close | Group::Inner | Group::Open | Group::Punct | Group::Rel
        )
    }
}

/// One `symbols[mode][name]` entry. `replace` is `None` for both `null` and `""` (every
/// use in KaTeX tests it for truthiness).
#[derive(Clone, Copy, Debug)]
pub struct CharInfo {
    pub font: SymbolFont,
    pub group: Group,
    pub replace: Option<&'static str>,
}

fn table(mode: Mode) -> &'static [(&'static str, CharInfo)] {
    match mode {
        Mode::Math => symbols_data::MATH,
        Mode::Text => symbols_data::TEXT,
    }
}

/// `symbols[mode][name]`.
pub fn get(mode: Mode, name: &str) -> Option<&'static CharInfo> {
    let t = table(mode);
    t.binary_search_by(|(k, _)| k.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|i| &t[i].1)
}

/// `Object.prototype.hasOwnProperty.call(symbols[mode], name)`.
pub fn has(mode: Mode, name: &str) -> bool {
    get(mode, name).is_some()
}

/// `ligatures` (symbols.ts): text-mode multi-character symbols.
pub fn is_ligature(text: &str) -> bool {
    matches!(text, "--" | "---" | "``" | "''")
}

/// `extraLatin`.
pub const EXTRA_LATIN: &str = "\u{00d0}\u{00de}\u{00fe}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_sorted() {
        for t in [symbols_data::MATH, symbols_data::TEXT] {
            for w in t.windows(2) {
                assert!(
                    w[0].0.as_bytes() < w[1].0.as_bytes(),
                    "{:?} !< {:?}",
                    w[0].0,
                    w[1].0
                );
            }
        }
    }
}
