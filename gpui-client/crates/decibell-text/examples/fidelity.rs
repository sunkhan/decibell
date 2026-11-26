//! The development view of the fidelity comparison: lays the corpus out with the engine and
//! prints where it differs from Chromium's dump (`tools/README.md`).
//!
//!   cargo run --release -p decibell-text --example fidelity -- corpus.json chromium.json [--show N] [--filter text]

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


    use std::collections::HashMap;
    use std::rc::Rc;

    use decibell_text::Layout;
    use serde_json::Value;

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let corpus: Value = serde_json::from_str(&std::fs::read_to_string(&args[0]).unwrap()).unwrap();
        let chromium: Value = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
        let show: usize = flag(&args, "--show").map_or(12, |value| value.parse().unwrap());
        let filter = flag(&args, "--filter");
        let scale = chromium["dsf"].as_f64().unwrap() as f32;
        let engine = common::engine();
        let styles: HashMap<String, common::BodyStyle> = corpus["styles"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| (name.clone(), common::BodyStyle::from_json(value)))
            .collect();

        let mut groups: HashMap<String, [usize; 2]> = HashMap::new();
        let (mut width_deltas, mut height_deltas, mut top_deltas) = (Vec::new(), Vec::new(), Vec::new());
        let mut shown = 0;
        let mut skipped = 0;
        let started = std::time::Instant::now();
        for (case, result) in corpus["cases"].as_array().unwrap().iter().zip(chromium["cases"].as_array().unwrap()) {
            let id = case["id"].as_str().unwrap();
            assert_eq!(id, result["id"].as_str().unwrap());
            if filter.as_ref().is_some_and(|filter| !id.contains(filter.as_str())) {
                continue;
            }
            let body = &styles[case["style"].as_str().unwrap()];
            let segs = case["segs"].as_array().unwrap();
            let wrap = case["wrap"].as_str().unwrap();
            let paragraph = Rc::new(common::paragraph(&engine, body, wrap, segs, scale));
            if args.iter().any(|arg| arg == "--dump") {
                let text = paragraph.text();
                let mut x = 0.;
                for (offset, advance, flags) in paragraph.debug_units() {
                    x += advance;
                    let utf16 = text[..offset as usize].encode_utf16().count();
                    println!("  {utf16:>4} {:?} +{advance:.4} = {x:.4} flags {flags}", text[offset as usize..].chars().next().unwrap());
                }
            }
            if args.iter().any(|arg| arg == "--edges") && wrap == "body" && id.starts_with('f') {
                // How full Chromium's lines are by our arithmetic, and how much the next
                // cluster would have overflowed: the decision boundary.
                let text = paragraph.text();
                let available = case["w"].as_f64().unwrap() as f32 * scale;
                let byte_of = |utf16: u64| {
                    let mut units = 0;
                    for (byte, c) in text.char_indices() {
                        if units >= utf16 { return byte as u32; }
                        units += c.len_utf16() as u64;
                    }
                    text.len() as u32
                };
                let lines = result["lines"].as_array().unwrap();
                for pair in lines.windows(2) {
                    let (from, to) = (byte_of(pair[0][0].as_u64().unwrap()), byte_of(pair[1][0].as_u64().unwrap()));
                    if text[from as usize..to as usize].ends_with('\n') || !segs.iter().all(|seg| seg[0].as_u64() == Some(0)) {
                        continue;
                    }
                    let (shaped, fit) = Layout::debug_line_width(&paragraph, from, to, &engine);
                    let next = to + text[to as usize..].chars().next().map_or(0, |c| c.len_utf8() as u32);
                    let (next_shaped, next_fit) = Layout::debug_line_width(&paragraph, from, next, &engine);
                    let first = pair[0][0].as_u64() == Some(0);
                    println!("EDGE {} taken {:.4} {:.4} refused {:.4} {:.4} {id}", if first { "first" } else { "later" },
                        shaped - available, fit - available, next_shaped - available, next_fit - available);
                }
            }
            let layout = Layout::new(paragraph, case["w"].as_f64().unwrap() as f32, &engine);
            let ours = common::lines(&layout, &engine, body, segs, scale);
            let theirs: Vec<(u32, f32, Option<f32>)> = result["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| (line[0].as_u64().unwrap() as u32, line[1].as_f64().unwrap() as f32, line[2].as_f64().map(|top| top as f32)))
                .collect();
            if !common::comparable(segs) {
                skipped += 1;
                continue;
            }
            let text = layout.paragraph().text();
            let same = common::line_starts(&ours, text, wrap) == common::line_starts(&theirs, text, wrap);
            let kind = format!("{}{} {wrap}", &id[..1], if id.starts_with('f') { "" } else { "and" });
            let group = groups.entry(kind).or_default();
            group[0] += 1;
            let height = result["h"].as_f64().unwrap() as f32;
            if same {
                group[1] += 1;
                height_deltas.push(((layout.height() - height).abs(), id));
                if (layout.height() - height).abs() > 0.01 && args.iter().any(|arg| arg == "--heights") && shown < show {
                    shown += 1;
                    println!("HEIGHT {id}: ours {} theirs {height} lines {:?}\n    theirs {:?}\n    segs {}", layout.height(), ours, theirs, case["segs"]);
                }
                for (a, b) in ours.iter().zip(&theirs) {
                    if b.1 > 0. {
                        width_deltas.push(((a.1 - b.1).abs(), id));
                        if (a.1 - b.1).abs() > 0.02 && args.iter().any(|arg| arg == "--widths") && shown < show {
                            shown += 1;
                            println!("WIDTH {id}: ours {} theirs {} (line at {})\n    segs {}", a.1, b.1, a.0, case["segs"]);
                        }
                    }
                    if let (Some(a), Some(b)) = (a.2, b.2) {
                        top_deltas.push(((a - b).abs(), id));
                    }
                }
            } else if shown < show {
                shown += 1;
                println!("BREAKS {id}: ours {:?}\n            theirs {:?}", starts(&ours), starts(&theirs));
                let text = layout.paragraph().text();
                println!("   text {:?}", &text[..text.char_indices().nth(160).map_or(text.len(), |(at, _)| at)]);
                if args.iter().any(|arg| arg == "--lines") {
                    let utf16: Vec<u16> = text.encode_utf16().collect();
                    for (name, lines) in [("ours", &ours), ("theirs", &theirs)] {
                        for (index, line) in lines.iter().enumerate() {
                            let end = lines.get(index + 1).map_or(utf16.len(), |next| next.0 as usize);
                            println!("     {name:<6} {:>8.3} {:?}", line.1, String::from_utf16_lossy(&utf16[line.0 as usize..end.max(line.0 as usize)]));
                        }
                    }
                }
            }
        }
        println!("laid out in {:.2} s; scale {scale}; {skipped} cases not comparable (right-to-left or fallback fonts)", started.elapsed().as_secs_f64());
        let mut names: Vec<_> = groups.keys().cloned().collect();
        names.sort();
        for name in names {
            let [total, same] = groups[&name];
            println!("{name:<14} identical line breaks {same}/{total} = {:.3} %", same as f64 * 100. / total as f64);
        }
        for (name, deltas) in [("line width", &mut width_deltas), ("height", &mut height_deltas), ("line top", &mut top_deltas)] {
            deltas.sort_by(|a, b| a.0.total_cmp(&b.0));
            if deltas.is_empty() {
                continue;
            }
            let at = |q: f64| deltas[((deltas.len() - 1) as f64 * q) as usize];
            println!(
                "{name:<11} n={} p50 {:.4} p99 {:.4} p99.9 {:.4} max {:.4} ({})",
                deltas.len(), at(0.5).0, at(0.99).0, at(0.999).0, at(1.0).0, at(1.0).1
            );
        }
    }

    fn starts(lines: &[(u32, f32, Option<f32>)]) -> Vec<(u32, f32)> {
        lines.iter().map(|line| (line.0, line.1)).collect()
    }

    fn flag(args: &[String], name: &str) -> Option<String> {
        let at = args.iter().position(|arg| arg == name)?;
        args.get(at + 1).cloned()
    }
}
