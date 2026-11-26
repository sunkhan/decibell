//! Port of KaTeX `src/Style.ts`: the eight TeX styles (D, D', T, T', S, S', SS, SS').

use crate::parse_node::StyleStr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub id: u8,
    pub size: u8,
    pub cramped: bool,
}

const D: u8 = 0;
const DC: u8 = 1;
const T: u8 = 2;
const TC: u8 = 3;
const S: u8 = 4;
const SC: u8 = 5;
const SS: u8 = 6;
const SSC: u8 = 7;

const STYLES: [Style; 8] = [
    Style {
        id: D,
        size: 0,
        cramped: false,
    },
    Style {
        id: DC,
        size: 0,
        cramped: true,
    },
    Style {
        id: T,
        size: 1,
        cramped: false,
    },
    Style {
        id: TC,
        size: 1,
        cramped: true,
    },
    Style {
        id: S,
        size: 2,
        cramped: false,
    },
    Style {
        id: SC,
        size: 2,
        cramped: true,
    },
    Style {
        id: SS,
        size: 3,
        cramped: false,
    },
    Style {
        id: SSC,
        size: 3,
        cramped: true,
    },
];

const SUP: [u8; 8] = [S, SC, S, SC, SS, SSC, SS, SSC];
const SUB: [u8; 8] = [SC, SC, SC, SC, SSC, SSC, SSC, SSC];
const FRAC_NUM: [u8; 8] = [T, TC, S, SC, SS, SSC, SS, SSC];
const FRAC_DEN: [u8; 8] = [TC, TC, SC, SC, SSC, SSC, SSC, SSC];
const CRAMP: [u8; 8] = [DC, DC, TC, TC, SC, SC, SSC, SSC];
const TEXT: [u8; 8] = [D, DC, T, TC, T, TC, T, TC];

impl Style {
    pub const DISPLAY: Style = STYLES[D as usize];
    pub const TEXT: Style = STYLES[T as usize];
    pub const SCRIPT: Style = STYLES[S as usize];
    pub const SCRIPTSCRIPT: Style = STYLES[SS as usize];

    pub fn sup(self) -> Style {
        STYLES[SUP[self.id as usize] as usize]
    }
    pub fn sub(self) -> Style {
        STYLES[SUB[self.id as usize] as usize]
    }
    pub fn frac_num(self) -> Style {
        STYLES[FRAC_NUM[self.id as usize] as usize]
    }
    pub fn frac_den(self) -> Style {
        STYLES[FRAC_DEN[self.id as usize] as usize]
    }
    pub fn cramp(self) -> Style {
        STYLES[CRAMP[self.id as usize] as usize]
    }
    pub fn text(self) -> Style {
        STYLES[TEXT[self.id as usize] as usize]
    }
    pub fn is_tight(self) -> bool {
        self.size >= 2
    }

    /// `styleMap[str]` (buildHTML / styling).
    pub fn from_style_str(s: StyleStr) -> Style {
        match s {
            StyleStr::Display => Style::DISPLAY,
            StyleStr::Text => Style::TEXT,
            StyleStr::Script => Style::SCRIPT,
            StyleStr::ScriptScript => Style::SCRIPTSCRIPT,
        }
    }
}
