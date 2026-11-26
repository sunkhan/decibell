//! Port of KaTeX `src/units.ts`.

use crate::dom_tree::CssVal;
use crate::options::Options;
use crate::parse_error::{Result, perr};
use crate::parse_node::Measurement;
use crate::utils::{js_num, to_fixed4};

fn pt_per_unit(unit: &str) -> Option<f64> {
    Some(match unit {
        "pt" => 1.0,
        "mm" => 7227.0 / 2540.0,
        "cm" => 7227.0 / 254.0,
        "in" => 72.27,
        "bp" => 803.0 / 800.0,
        "pc" => 12.0,
        "dd" => 1238.0 / 1157.0,
        "cc" => 14856.0 / 1157.0,
        "nd" => 685.0 / 642.0,
        "nc" => 1370.0 / 107.0,
        "sp" => 1.0 / 65536.0,
        "px" => 803.0 / 800.0,
        _ => return None,
    })
}

/// `validUnit(unit)`, returning the interned unit.
pub fn valid_unit(unit: &str) -> Option<&'static str> {
    const UNITS: [&str; 15] = [
        "pt", "mm", "cm", "in", "bp", "pc", "dd", "cc", "nd", "nc", "sp", "px", "ex", "em", "mu",
    ];
    UNITS.iter().copied().find(|u| *u == unit)
}

/// `calculateSize(sizeValue, options)`.
pub fn calculate_size(size: &Measurement, options: &Options) -> Result<f64> {
    let scale;
    if let Some(pt) = pt_per_unit(size.unit) {
        scale = pt / options.font_metrics().pt_per_em / options.size_multiplier;
    } else if size.unit == "mu" {
        scale = options.font_metrics().css_em_per_mu;
    } else {
        // `unitOptions !== options` exactly when havingStyle changed the style.
        let tight = options.style.is_tight();
        let unit_options = if tight {
            options.having_style(options.style.text())
        } else {
            options.clone()
        };
        let changed = tight && options.style.text() != options.style;
        let mut s = if size.unit == "ex" {
            unit_options.font_metrics().x_height
        } else if size.unit == "em" {
            unit_options.font_metrics().quad
        } else {
            return perr(format!("Invalid unit: '{}'", size.unit), None);
        };
        if changed {
            s *= unit_options.size_multiplier / options.size_multiplier;
        }
        scale = s;
    }
    Ok((size.number * scale).min(options.max_size))
}

/// `makeEm(n)` as a style value.
pub fn make_em(n: f64) -> CssVal {
    CssVal::Em(to_fixed4(n))
}

/// `makeEm(n)` as a string (for attribute values built by concatenation).
pub fn make_em_str(n: f64) -> String {
    let mut s = js_num(to_fixed4(n));
    s.push_str("em");
    s
}
