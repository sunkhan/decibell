//! Prints the display list for one formula: `layout_dump [--display] 'tex'`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let display = args.iter().any(|a| a == "--display");
    let tex = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    let r = decibell_tex::layout(&tex, display, &decibell_tex::LayoutOptions::default());
    match r {
        decibell_tex::Rendered::Math(m) => {
            println!("width {:.4} lines {:?}", m.width, m.lines);
            for it in m.items.iter().chain(m.tag_items.iter()) {
                match it {
                    decibell_tex::Item::Glyph(g) => println!(
                        "glyph {:?} {:?} x {:.4} y {:.4} size {:.4}",
                        g.ch, g.font, g.x, g.y, g.size
                    ),
                    decibell_tex::Item::Rect { x, y, w, h, .. } => {
                        println!("rect {:.4} {:.4} {:.4} {:.4}", x, y, w, h)
                    }
                    decibell_tex::Item::Path(p) => println!(
                        "svg {:.4} {:.4} {:.4} {:.4} clip {:?}",
                        p.x, p.y, p.w, p.h, p.clip
                    ),
                }
            }
        }
        other => println!("{:?}", other),
    }
}
