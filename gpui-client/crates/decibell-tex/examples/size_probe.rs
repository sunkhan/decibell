//! Binary-size probe: lays out the formula given on the command line (with `--empty`, does
//! nothing, for the baseline). Build both in release and compare.
fn main() {
    let tex = std::env::args().nth(1).unwrap_or_default();
    if tex == "--empty" {
        println!("empty");
        return;
    }
    let r = decibell_tex::layout(&tex, false, &decibell_tex::LayoutOptions::default());
    if let decibell_tex::Rendered::Math(m) = r {
        println!("{}", m.items.len());
    }
}
