//! Times parse + build + layout over a corpus: `bench <corpus.jsonl>` (release build).

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("corpus.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut items = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        items.push((
            v["id"].as_str().unwrap().to_string(),
            v["tex"].as_str().unwrap().to_string(),
            v["display"].as_bool().unwrap(),
        ));
    }
    let opts = decibell_tex::LayoutOptions::default();
    let mut times: Vec<(f64, String, usize)> = Vec::new();
    for (id, tex, display) in &items {
        let mut best = f64::INFINITY;
        let mut n = 0;
        for _ in 0..5 {
            let t = Instant::now();
            let r = decibell_tex::layout(tex, *display, &opts);
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
            if let decibell_tex::Rendered::Math(m) = &r {
                n = m.items.len();
            }
        }
        times.push((best, id.clone(), n));
    }
    let mut ms: Vec<f64> = times.iter().map(|t| t.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| ms[((ms.len() as f64 - 1.0) * q) as usize];
    println!(
        "{} formulas: median {:.4} ms, p90 {:.4} ms, p99 {:.4} ms, max {:.3} ms, total {:.1} ms",
        ms.len(),
        p(0.5),
        p(0.9),
        p(0.99),
        p(1.0),
        ms.iter().sum::<f64>()
    );
    times.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for t in times.iter().take(6) {
        println!("  {:.3} ms {} ({} items)", t.0, t.1, t.2);
    }
    // A typical item size.
    println!(
        "size_of Item = {} bytes, Glyph = {} bytes",
        std::mem::size_of::<decibell_tex::Item>(),
        std::mem::size_of::<decibell_tex::layout::Glyph>()
    );
}
