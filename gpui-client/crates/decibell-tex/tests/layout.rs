//! Layout differential test against KaTeX rendered by Chromium (Electron 33, DSF 2, host font
//! 16 px): `tests/data/layout.jsonl.gz` from `tools/harness`. Compares every glyph's pen
//! position and baseline, every painted rule / border, every SVG viewport, and the formula and
//! base widths.
//!
//! Chromium reports glyph lefts floored to its LayoutUnit grid (1/128 CSS px at DSF 2); our
//! positions are the unsnapped pen positions, so a glyph matches exactly when
//! `floor(ours·128) == measured·128`. The tolerance for "within bound" is 0.02 CSS px.
//!
//! `TEX_LAYOUT_VERBOSE=1` prints the worst formulas; `TEX_LAYOUT_FILTER=<substring of id>`
//! restricts the run.

use std::io::{BufRead, BufReader};

use decibell_tex::layout::{Item, LayoutOptions, Rendered, layout};

const TOL: f64 = 0.02;
const GRID: f64 = 128.0;

fn data(name: &str) -> std::path::PathBuf {
    let here = std::path::Path::new(file!());
    let root = if here.is_absolute() {
        here.parent().unwrap().parent().unwrap().to_path_buf()
    } else {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };
    root.join("tests/data").join(name)
}

struct Glyph {
    ch: char,
    x: f64,
    y: f64,
}

#[derive(Default)]
struct Stats {
    formulas: usize,
    compared: usize,
    exact: usize,
    within: usize,
    glyphs: usize,
    glyphs_exact: usize,
    glyphs_within: usize,
    glyph_count_mismatch: usize,
    rects: usize,
    rects_within: usize,
    svgs: usize,
    svgs_within: usize,
    width_within: usize,
    max_dev: f64,
    worst: Vec<(f64, String, String)>,
    statuses: [usize; 4],
    fallback: usize,
    wrapped: usize,
    width_fail: usize,
    status_ids: Vec<String>,
}

#[test]
fn layout_matches_chromium() {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    // `TEX_LAYOUT_DATA=<file>` compares against another harness output (calibration runs).
    let path = std::env::var("TEX_LAYOUT_DATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| data("layout.jsonl.gz"));
    let f = std::fs::File::open(path).expect("layout vectors");
    let r = BufReader::new(flate2::read::GzDecoder::new(f));
    let verbose = std::env::var("TEX_LAYOUT_VERBOSE").is_ok();
    let filter = std::env::var("TEX_LAYOUT_FILTER").ok();
    let opts = LayoutOptions::default();
    let mut st = Stats::default();
    // `TEX_LAYOUT_DUMP=<file>`: one line per compared glyph: id, char, dx, dy (LayoutUnits).
    let mut dump = std::env::var("TEX_LAYOUT_DUMP")
        .ok()
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    for (i, line) in r.lines().enumerate() {
        if i == 0 {
            continue; // meta
        }
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        if let Some(f) = &filter
            && !id.contains(f.as_str())
        {
            continue;
        }
        st.formulas += 1;
        let tex = v["tex"].as_str().unwrap();
        let display = v["display"].as_bool().unwrap();
        let status = v["status"].as_str().unwrap();
        let ours = layout(tex, display, &opts);
        match (status, &ours) {
            ("ok", Rendered::Math(_)) => {}
            ("error", Rendered::Error { .. }) => {
                st.statuses[1] += 1;
                continue;
            }
            ("throw", Rendered::Internal { .. }) => {
                st.statuses[2] += 1;
                continue;
            }
            _ => {
                st.statuses[3] += 1;
                st.status_ids.push(id.clone());
                if verbose {
                    eprintln!(
                        "STATUS {} {:?}: katex {} ours {:?}",
                        id,
                        tex,
                        status,
                        std::mem::discriminant(&ours)
                    );
                }
                continue;
            }
        }
        st.statuses[0] += 1;
        let Rendered::Math(m) = ours else {
            unreachable!()
        };
        // Chromium wrapped the formula between bases (wider than the 20000 px host): the
        // caller does that wrapping (`MathLayout::bases`), the layout itself is unwrapped.
        let bases = v["bases"].as_array().unwrap();
        let _ = bases;
        if !display && m.width > 19_990.0 {
            st.wrapped += 1;
            continue;
        }
        st.compared += 1;
        // Measured glyphs (skip transparent ones: phantoms are not drawn).
        let mut theirs = Vec::new();
        for g in v["glyphs"].as_array().unwrap() {
            let c = g["c"].as_str().unwrap_or("");
            if c == "#00000000" || c.contains("rgba(0, 0, 0, 0)") || c == "transparent" {
                continue;
            }
            let t = g["t"].as_str().unwrap();
            // Default-ignorable code points (U+FE0F, …) draw nothing.
            if matches!(
                t.chars().next(),
                Some('\u{200b}'..='\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{2060}' | '\u{feff}')
            ) {
                continue;
            }
            theirs.push(Glyph {
                ch: t.chars().next().unwrap(),
                x: g["x"].as_f64().unwrap(),
                y: g["y"].as_f64().unwrap(),
            });
        }
        let mut mine = Vec::new();
        // A display tag sits at the container's right edge (`right: 0`; the harness container
        // is 1000 px wide).
        let tag_dx = 1000.0 - m.tag_width;
        let tag_items = m.tag_items.iter().map(|it| match it {
            Item::Glyph(g) => {
                let mut g = *g;
                g.x += tag_dx;
                Item::Glyph(g)
            }
            other => other.clone(),
        });
        let all: Vec<Item> = m.items.iter().cloned().chain(tag_items).collect();
        let mut fallback_formula = false;
        for it in all.iter() {
            if let Item::Glyph(g) = it {
                if g.fallback {
                    fallback_formula = true;
                }
                // `\pmb` shadows and CSS-generated equation numbers are not text nodes.
                if g.shadow || g.generated {
                    continue;
                }
                mine.push(Glyph {
                    ch: g.ch,
                    x: g.x as f64,
                    y: g.y as f64,
                });
            }
        }
        if fallback_formula {
            // Glyphs Chromium draws with a system font: their advances are unknowable here.
            st.fallback += 1;
            continue;
        }
        if theirs.is_empty() && !mine.is_empty() && v["box"]["w"].as_f64().unwrap_or(0.0) > 0.0 {
            // The harness recorded no text nodes for a non-empty formula (two records whose
            // U+203C made Chromium pick an emoji font for the whole run); nothing to compare.
            st.fallback += 1;
            continue;
        }
        let mut max_dev = 0.0f64;
        let mut all_exact = true;
        let mut detail = String::new();
        if mine.len() != theirs.len() {
            st.glyph_count_mismatch += 1;
            all_exact = false;
            max_dev = f64::INFINITY;
            detail = format!("glyph count ours {} chromium {}", mine.len(), theirs.len());
        } else {
            // Match in order when the character sequences agree, else greedily by char.
            let same_order = mine.iter().zip(&theirs).all(|(a, b)| a.ch == b.ch);
            let mut used = vec![false; theirs.len()];
            for (k, g) in mine.iter().enumerate() {
                let j = if same_order {
                    k
                } else {
                    let mut best = None;
                    let mut bd = f64::INFINITY;
                    for (j, t) in theirs.iter().enumerate() {
                        if !used[j] && t.ch == g.ch {
                            let d = (t.x - g.x).abs() + (t.y - g.y).abs();
                            if d < bd {
                                bd = d;
                                best = Some(j);
                            }
                        }
                    }
                    match best {
                        Some(j) => j,
                        None => {
                            max_dev = f64::INFINITY;
                            all_exact = false;
                            continue;
                        }
                    }
                };
                used[j] = true;
                let t = &theirs[j];
                st.glyphs += 1;
                // Chromium's Range left is the pen position floored to the grid, minus an
                // epsilon inside a run: a pen position exactly on the grid reads either as
                // itself (run start) or one LayoutUnit lower (later glyphs).
                let xs = g.x * GRID;
                let dx = if (xs - xs.round()).abs() < 1e-3 {
                    let a = xs.round() / GRID - t.x;
                    let b = (xs.round() - 1.0) / GRID - t.x;
                    if a.abs() <= b.abs() { a } else { b }
                } else {
                    xs.floor() / GRID - t.x
                };
                let dy = g.y - t.y;
                let dev = dx.abs().max(dy.abs());
                if let Some(d) = dump.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(
                        d,
                        "{}\t{}\t{}\t{}",
                        id,
                        g.ch,
                        (dx * GRID).round(),
                        (dy * GRID).round()
                    );
                }
                let exact = dx.abs() < 1e-6 && dy.abs() < 1e-6;
                if exact {
                    st.glyphs_exact += 1;
                } else {
                    all_exact = false;
                }
                if dev <= TOL {
                    st.glyphs_within += 1;
                }
                if dev > max_dev {
                    max_dev = dev;
                    detail = format!(
                        "{:?} ours ({:.4},{:.4}) chromium ({:.4},{:.4})",
                        g.ch, g.x, g.y, t.x, t.y
                    );
                }
            }
        }
        // Rules / borders: each measured painted border side against our rects.
        let mut rects: Vec<(f64, f64, f64, f64)> = Vec::new();
        for it in m.items.iter() {
            if let Item::Rect { x, y, w, h, .. } = it {
                rects.push((*x as f64, *y as f64, *w as f64, *h as f64));
            }
        }
        for r in v["rects"].as_array().unwrap() {
            let (x, y, w, h) = (
                r["x"].as_f64().unwrap(),
                r["y"].as_f64().unwrap(),
                r["w"].as_f64().unwrap(),
                r["h"].as_f64().unwrap(),
            );
            let mut want = Vec::new();
            if let Some(b) = r["border"].as_object() {
                for (side, val) in b {
                    let bw = val["w"].as_f64().unwrap_or(0.0);
                    let bc = val["c"].as_str().unwrap_or("");
                    if bw <= 0.0 || bc == "#00000000" || val["s"].as_str() == Some("none") {
                        continue;
                    }
                    want.push(match side.as_str() {
                        "t" => (x, y, w, bw),
                        "b" => (x, y + h - bw, w, bw),
                        "l" => (x, y, bw, h),
                        _ => (x + w - bw, y, bw, h),
                    });
                }
            }
            if r["bg"]
                .as_str()
                .map(|s| !s.is_empty() && s != "rgba(0, 0, 0, 0)" && s != "#00000000")
                .unwrap_or(false)
            {
                want.push((x, y, w, h));
            }
            for wr in want {
                if wr.2 <= 0.0 || wr.3 <= 0.0 {
                    continue; // paints nothing
                }
                st.rects += 1;
                let best = rects
                    .iter()
                    .map(|o| {
                        (o.0 - wr.0)
                            .abs()
                            .max((o.1 - wr.1).abs())
                            .max((o.2 - wr.2).abs())
                            .max((o.3 - wr.3).abs())
                    })
                    .fold(f64::INFINITY, f64::min);
                if best > TOL && std::env::var("TEX_LAYOUT_SHAPES").is_ok() {
                    eprintln!(
                        "RECT {} {:?} want {:?} best {:.4}",
                        id,
                        tex.chars().take(50).collect::<String>(),
                        wr,
                        best
                    );
                }
                if best <= TOL {
                    st.rects_within += 1;
                } else {
                    all_exact = false;
                    if best > max_dev {
                        max_dev = best;
                        detail = format!("rect {:?} off by {:.4}", wr, best);
                    }
                }
            }
        }
        // SVG viewports.
        let mut paths = Vec::new();
        for it in m.items.iter() {
            if let Item::Path(p) = it {
                paths.push((p.x as f64, p.y as f64, p.w as f64, p.h as f64));
            }
        }
        for s in v["svgs"].as_array().unwrap() {
            let (x, y, w, h) = (
                s["x"].as_f64().unwrap(),
                s["y"].as_f64().unwrap(),
                s["w"].as_f64().unwrap(),
                s["h"].as_f64().unwrap(),
            );
            st.svgs += 1;
            let best = paths
                .iter()
                .map(|o| {
                    (o.0 - x)
                        .abs()
                        .max((o.1 - y).abs())
                        .max((o.2 - w).abs())
                        .max((o.3 - h).abs())
                })
                .fold(f64::INFINITY, f64::min);
            if best > TOL && std::env::var("TEX_LAYOUT_SHAPES").is_ok() {
                eprintln!(
                    "SVG {} {:?} want ({:.4},{:.4},{:.4},{:.4}) best {:.4}",
                    id,
                    tex.chars().take(50).collect::<String>(),
                    x,
                    y,
                    w,
                    h,
                    best
                );
            }
            if best <= TOL {
                st.svgs_within += 1;
            } else {
                all_exact = false;
                if best > max_dev {
                    max_dev = best;
                    detail = format!(
                        "svg ({:.4},{:.4},{:.4},{:.4}) off by {:.4}",
                        x, y, w, h, best
                    );
                }
            }
        }
        // Formula width (inline: the .katex-html box).
        if !display {
            let w = v["box"]["w"].as_f64().unwrap();
            let d = (m.width as f64 - w).abs();
            if d <= TOL {
                st.width_within += 1;
            } else {
                st.width_fail += 1;
                if d > max_dev {
                    max_dev = d;
                    detail = format!("width ours {:.4} chromium {:.4}", m.width, w);
                }
            }
        } else {
            st.width_within += 1;
        }
        if all_exact {
            st.exact += 1;
        }
        if max_dev <= TOL {
            st.within += 1;
        }
        if max_dev.is_finite() {
            st.max_dev = st.max_dev.max(max_dev);
        }
        st.worst.push((
            max_dev,
            id,
            format!("{:?}: {}", tex.chars().take(70).collect::<String>(), detail),
        ));
    }
    st.worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    if verbose {
        for w in st.worst.iter().take(60) {
            eprintln!("{:>9.4} {} {}", w.0, w.1, w.2);
        }
    }
    eprintln!(
        "layout: {} formulas; status ok {} / error {} / internal {} / status mismatch {}",
        st.formulas, st.statuses[0], st.statuses[1], st.statuses[2], st.statuses[3]
    );
    eprintln!(
        "layout: {} formulas use system-font fallback glyphs, {} were wrapped by Chromium (not compared)\nlayout: of {} compared: {} all-exact, {} within {} px; glyphs {} (exact {}, within {}), {} glyph-count mismatches; rects {}/{}; svgs {}/{}; widths {}/{} (off: {}); max finite deviation {:.4} px",
        st.fallback,
        st.wrapped,
        st.compared - st.fallback - st.wrapped,
        st.exact,
        st.within,
        TOL,
        st.glyphs,
        st.glyphs_exact,
        st.glyphs_within,
        st.glyph_count_mismatch,
        st.rects_within,
        st.rects,
        st.svgs_within,
        st.svgs,
        st.width_within,
        st.compared,
        st.width_fail,
        st.max_dev
    );

    if filter.is_some() || std::env::var("TEX_LAYOUT_DATA").is_ok() {
        return;
    }
    // Regression bounds (the state this port reached; see STATUS.md). Statuses differ only for
    // the two brace bombs beyond MAX_DEPTH.
    assert!(
        st.status_ids.iter().all(|i| i.starts_with("adv-braces-")),
        "status mismatches: {:?}",
        st.status_ids
    );
    let compared = st.compared - st.fallback - st.wrapped;
    assert!(
        st.within * 1000 >= compared * 960,
        "only {}/{} formulas within {} px",
        st.within,
        compared,
        TOL
    );
    assert!(
        st.width_fail == 0,
        "{} formula widths off by more than {} px",
        st.width_fail,
        TOL
    );
    assert!(st.max_dev <= 0.25, "max deviation {:.4} px", st.max_dev);
    assert!(
        st.glyph_count_mismatch <= 5,
        "glyph count mismatches: {}",
        st.glyph_count_mismatch
    );
}
