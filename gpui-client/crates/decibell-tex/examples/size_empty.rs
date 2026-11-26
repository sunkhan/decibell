//! Binary-size baseline for `size_probe` (does not use decibell-tex).
fn main() {
    let tex = std::env::args().nth(1).unwrap_or_default();
    println!("{}", tex.len());
}
