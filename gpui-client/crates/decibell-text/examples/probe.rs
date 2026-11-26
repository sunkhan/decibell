//! Prints how the engine shapes its arguments: the caret units (text offset, advance, flags)
//! and the layout's size. A development aid.
//!
//!   cargo run -p decibell-text --example probe -- "some text" "مرحبا"

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "../tests/common/mod.rs"]
mod common;

fn main() {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    linux::main();
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    eprintln!("this tool builds GPUI's Linux text system: run it on Linux");
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use super::common;


    use std::rc::Rc;

    use decibell_text::{FontSpec, Layout, ParagraphBuilder, TextStyle, Wrapping};

    pub fn main() {
        let engine = common::engine();
        let face = engine.face(&FontSpec::new("Decibell Sans", 400));
        let style = TextStyle::new(face, 14.5, gpui::Hsla::default());
        for text in std::env::args().skip(1) {
            let mut builder = ParagraphBuilder::new(&engine, &style, Wrapping::NOWRAP, 1.);
            builder.text(&text, &style, 0);
            let paragraph = Rc::new(builder.build());
            let layout = Layout::new(paragraph.clone(), f32::INFINITY, &engine);
            println!("{text:?}: {} × {} px", layout.width(), layout.height());
            for (offset, advance, flags) in paragraph.debug_units() {
                let character = text[offset as usize..].chars().next().unwrap_or(' ');
                println!("  {offset:>4} {character:?} advance {advance:.4} flags {flags:#b}");
            }
        }
    }
}
