//! Reads JSON lines `{"tex": …, "display": bool}` (or plain TeX lines with `--plain`) on stdin
//! and prints, per line, the markup `render_to_markup` produces (`!INTERNAL: msg` for an
//! internal error). Used by tools/ to diff against KaTeX.

use std::io::{BufRead, Write};

fn main() {
    let plain = std::env::args().any(|a| a == "--plain");
    let display = std::env::args().any(|a| a == "--display");
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        let (tex, disp) = if plain {
            (line.clone(), display)
        } else {
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            (
                v["tex"].as_str().unwrap().to_string(),
                v["display"].as_bool().unwrap_or(false),
            )
        };
        let settings = decibell_tex::Settings {
            display_mode: disp,
            ..Default::default()
        };
        let r = match decibell_tex::render_to_markup(&tex, &settings) {
            Ok(m) => m,
            Err(e) => format!("!INTERNAL: {}", e),
        };
        writeln!(out, "{}", serde_json::Value::String(r)).unwrap();
    }
}
