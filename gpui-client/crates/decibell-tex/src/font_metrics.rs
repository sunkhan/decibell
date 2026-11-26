//! Port of KaTeX `src/fontMetrics.ts` (the data is `data/font_metrics_data.rs`).

use crate::data::font_metrics_data;
use crate::fonts::FontName;
use crate::parse_node::Mode;
use crate::unicode_scripts::supported_codepoint;
use crate::utils::first_code_unit;

/// `CharacterMetrics`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterMetrics {
    pub depth: f64,
    pub height: f64,
    pub italic: f64,
    pub skew: f64,
    pub width: f64,
}

/// `FontMetrics` (the sigmas and xis of one size class).
#[derive(Clone, Copy, Debug)]
pub struct FontMetrics {
    pub css_em_per_mu: f64,
    pub slant: f64,
    pub space: f64,
    pub stretch: f64,
    pub shrink: f64,
    pub x_height: f64,
    pub quad: f64,
    pub extra_space: f64,
    pub num1: f64,
    pub num2: f64,
    pub num3: f64,
    pub denom1: f64,
    pub denom2: f64,
    pub sup1: f64,
    pub sup2: f64,
    pub sup3: f64,
    pub sub1: f64,
    pub sub2: f64,
    pub sup_drop: f64,
    pub sub_drop: f64,
    pub delim1: f64,
    pub delim2: f64,
    pub axis_height: f64,
    pub default_rule_thickness: f64,
    pub big_op_spacing1: f64,
    pub big_op_spacing2: f64,
    pub big_op_spacing3: f64,
    pub big_op_spacing4: f64,
    pub big_op_spacing5: f64,
    pub sqrt_rule_thickness: f64,
    pub pt_per_em: f64,
    pub double_rule_sep: f64,
    pub array_rule_width: f64,
    pub fboxsep: f64,
    pub fboxrule: f64,
}

const fn metrics(i: usize) -> FontMetrics {
    const QUAD: [f64; 3] = [1.000, 1.171, 1.472];
    macro_rules! s {
        ($a:expr, $b:expr, $c:expr) => {
            [$a, $b, $c][i]
        };
    }
    FontMetrics {
        css_em_per_mu: QUAD[i] / 18.0,
        slant: s!(0.250, 0.250, 0.250),
        space: s!(0.000, 0.000, 0.000),
        stretch: s!(0.000, 0.000, 0.000),
        shrink: s!(0.000, 0.000, 0.000),
        x_height: s!(0.431, 0.431, 0.431),
        quad: QUAD[i],
        extra_space: s!(0.000, 0.000, 0.000),
        num1: s!(0.677, 0.732, 0.925),
        num2: s!(0.394, 0.384, 0.387),
        num3: s!(0.444, 0.471, 0.504),
        denom1: s!(0.686, 0.752, 1.025),
        denom2: s!(0.345, 0.344, 0.532),
        sup1: s!(0.413, 0.503, 0.504),
        sup2: s!(0.363, 0.431, 0.404),
        sup3: s!(0.289, 0.286, 0.294),
        sub1: s!(0.150, 0.143, 0.200),
        sub2: s!(0.247, 0.286, 0.400),
        sup_drop: s!(0.386, 0.353, 0.494),
        sub_drop: s!(0.050, 0.071, 0.100),
        delim1: s!(2.390, 1.700, 1.980),
        delim2: s!(1.010, 1.157, 1.420),
        axis_height: s!(0.250, 0.250, 0.250),
        default_rule_thickness: s!(0.04, 0.049, 0.049),
        big_op_spacing1: s!(0.111, 0.111, 0.111),
        big_op_spacing2: s!(0.166, 0.166, 0.166),
        big_op_spacing3: s!(0.2, 0.2, 0.2),
        big_op_spacing4: s!(0.6, 0.611, 0.611),
        big_op_spacing5: s!(0.1, 0.143, 0.143),
        sqrt_rule_thickness: s!(0.04, 0.04, 0.04),
        pt_per_em: s!(10.0, 10.0, 10.0),
        double_rule_sep: s!(0.2, 0.2, 0.2),
        array_rule_width: s!(0.04, 0.04, 0.04),
        fboxsep: s!(0.3, 0.3, 0.3),
        fboxrule: s!(0.04, 0.04, 0.04),
    }
}

static GLOBAL: [FontMetrics; 3] = [metrics(0), metrics(1), metrics(2)];

/// `getGlobalMetrics(size)`.
pub fn get_global_metrics(size: u8) -> &'static FontMetrics {
    let i = if size >= 5 {
        0
    } else if size >= 3 {
        1
    } else {
        2
    };
    &GLOBAL[i]
}

/// `extraCharacterMap`.
fn extra_character(c: char) -> Option<char> {
    Some(match c {
        'Å' => 'A',
        'Ð' => 'D',
        'Þ' => 'o',
        'å' => 'a',
        'ð' => 'd',
        'þ' => 'o',
        'А' => 'A',
        'Б' => 'B',
        'В' => 'B',
        'Г' => 'F',
        'Д' => 'A',
        'Е' => 'E',
        'Ж' => 'K',
        'З' => '3',
        'И' => 'N',
        'Й' => 'N',
        'К' => 'K',
        'Л' => 'N',
        'М' => 'M',
        'Н' => 'H',
        'О' => 'O',
        'П' => 'N',
        'Р' => 'P',
        'С' => 'C',
        'Т' => 'T',
        'У' => 'y',
        'Ф' => 'O',
        'Х' => 'X',
        'Ц' => 'U',
        'Ч' => 'h',
        'Ш' => 'W',
        'Щ' => 'W',
        'Ъ' => 'B',
        'Ы' => 'X',
        'Ь' => 'B',
        'Э' => '3',
        'Ю' => 'X',
        'Я' => 'R',
        'а' => 'a',
        'б' => 'b',
        'в' => 'a',
        'г' => 'r',
        'д' => 'y',
        'е' => 'e',
        'ж' => 'm',
        'з' => 'e',
        'и' => 'n',
        'й' => 'n',
        'к' => 'n',
        'л' => 'n',
        'м' => 'm',
        'н' => 'n',
        'о' => 'o',
        'п' => 'n',
        'р' => 'p',
        'с' => 'c',
        'т' => 'o',
        'у' => 'y',
        'ф' => 'b',
        'х' => 'x',
        'ц' => 'n',
        'ч' => 'n',
        'ш' => 'w',
        'щ' => 'w',
        'ъ' => 'a',
        'ы' => 'm',
        'ь' => 'a',
        'э' => 'e',
        'ю' => 'm',
        'я' => 'r',
        _ => return None,
    })
}

fn lookup(font: FontName, code: u16) -> Option<CharacterMetrics> {
    let t = font_metrics_data::table(font);
    let i = t.binary_search_by_key(&code, |e| e.0).ok()?;
    let m = t[i].1;
    let f = |v: i32| v as f64 / 100000.0;
    Some(CharacterMetrics {
        depth: f(m[0]),
        height: f(m[1]),
        italic: f(m[2]),
        skew: f(m[3]),
        width: f(m[4]),
    })
}

/// `getCharacterMetrics(character, font, mode)`.
pub fn get_character_metrics(
    character: &str,
    font: FontName,
    mode: Mode,
) -> Option<CharacterMetrics> {
    let mut ch = first_code_unit(character)?;
    let mut metrics = lookup(font, ch);
    if metrics.is_none() {
        // `character[0] in extraCharacterMap` (a BMP character)
        if let Some(c0) = character.chars().next().filter(|c| (*c as u32) <= 0xFFFF)
            && let Some(m) = extra_character(c0)
        {
            ch = m as u16;
            metrics = lookup(font, ch);
        }
    }
    if metrics.is_none() && mode == Mode::Text && supported_codepoint(ch as u32) {
        metrics = lookup(font, 77); // 'M'
    }
    metrics
}
