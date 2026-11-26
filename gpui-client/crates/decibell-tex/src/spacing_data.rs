//! Port of KaTeX `src/spacingData.ts`: inter-atom spacing by class pair.

use crate::parse_node::Measurement;

const THIN: Measurement = Measurement::new(3.0, "mu");
const MEDIUM: Measurement = Measurement::new(4.0, "mu");
const THICK: Measurement = Measurement::new(5.0, "mu");

/// `spacings[prev][next]`.
pub fn spacing(prev: &str, next: &str) -> Option<Measurement> {
    Some(match (prev, next) {
        ("mord", "mop") => THIN,
        ("mord", "mbin") => MEDIUM,
        ("mord", "mrel") => THICK,
        ("mord", "minner") => THIN,
        ("mop", "mord") => THIN,
        ("mop", "mop") => THIN,
        ("mop", "mrel") => THICK,
        ("mop", "minner") => THIN,
        ("mbin", "mord") => MEDIUM,
        ("mbin", "mop") => MEDIUM,
        ("mbin", "mopen") => MEDIUM,
        ("mbin", "minner") => MEDIUM,
        ("mrel", "mord") => THICK,
        ("mrel", "mop") => THICK,
        ("mrel", "mopen") => THICK,
        ("mrel", "minner") => THICK,
        ("mclose", "mop") => THIN,
        ("mclose", "mbin") => MEDIUM,
        ("mclose", "mrel") => THICK,
        ("mclose", "minner") => THIN,
        ("mpunct", "mord") => THIN,
        ("mpunct", "mop") => THIN,
        ("mpunct", "mrel") => THICK,
        ("mpunct", "mopen") => THIN,
        ("mpunct", "mclose") => THIN,
        ("mpunct", "mpunct") => THIN,
        ("mpunct", "minner") => THIN,
        ("minner", "mord") => THIN,
        ("minner", "mop") => THIN,
        ("minner", "mbin") => MEDIUM,
        ("minner", "mrel") => THICK,
        ("minner", "mopen") => THIN,
        ("minner", "mpunct") => THIN,
        ("minner", "minner") => THIN,
        _ => return None,
    })
}

/// `tightSpacings[prev][next]`.
pub fn tight_spacing(prev: &str, next: &str) -> Option<Measurement> {
    Some(match (prev, next) {
        ("mord", "mop")
        | ("mop", "mord")
        | ("mop", "mop")
        | ("mclose", "mop")
        | ("minner", "mop") => THIN,
        _ => return None,
    })
}
