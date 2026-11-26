//! The differential test: every case of tests/vectors/*.tsv.gz (lowlight's
//! output, written by tools/vectors.mjs) must come out of this crate as the
//! same tree, span for span.
//!
//! The committed sets run always. The large fuzz set
//! (tests/vectors/full/fuzz-full.tsv.gz, made by `node tools/vectors.mjs
//! --full`) runs when the file exists.

mod common;

use std::collections::BTreeMap;

use common::{crate_dir, ours, read_cases, Case};
use decibell_highlight::Language;

#[derive(Default)]
struct Tally {
    cases: usize,
    mismatches: usize,
    throws: usize,
    text: usize,
}

fn first_difference(a: &str, b: &str) -> String {
    let (ta, tb): (Vec<&str>, Vec<&str>) = (a.split_inclusive(';').collect(), b.split_inclusive(';').collect());
    let i = ta.iter().zip(&tb).take_while(|(x, y)| x == y).count();
    let ctx = |t: &[&str]| t[i.saturating_sub(3)..t.len().min(i + 4)].concat();
    format!("token {i}: lowlight …{}… ours …{}…", ctx(&ta), ctx(&tb))
}

fn run(file: &str) -> (BTreeMap<String, Tally>, Vec<String>) {
    let cases: Vec<Case> = read_cases(&crate_dir().join(file));
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut failures = Vec::new();
    for case in &cases {
        let grammar = Language::from_name(&case.lang).map_or("(none)", |l| l.name());
        let tally = tallies.entry(grammar.to_string()).or_default();
        tally.cases += 1;
        // lowlight's own output does not spell the input here (README): with
        // the same length the tree is still comparable, otherwise not
        if case.flag == "textlen" {
            tally.text += 1;
            continue;
        }
        let expected_flag = if case.flag == "text" {
            tally.text += 1;
            "ok"
        } else {
            case.flag.as_str()
        };
        if case.flag == "throw" {
            tally.throws += 1;
        }
        let (flag, tree, relevance) = ours(&case.lang, &case.code);
        // relevance drives sub-language detection; it must agree to the bit
        let relevance_same = case.relevance.parse::<f64>().is_ok_and(|r| r == relevance);
        if flag != expected_flag || tree != case.tree || !relevance_same {
            tally.mismatches += 1;
            if failures.len() < 12 {
                failures.push(format!(
                    "{} [{}] flags {}/{}, relevance {}/{}: {}\n    code: {:?}",
                    case.source,
                    case.lang,
                    case.flag,
                    flag,
                    case.relevance,
                    relevance,
                    first_difference(&case.tree, &tree),
                    String::from_utf16_lossy(&case.code).chars().take(300).collect::<String>(),
                ));
            }
        }
    }
    (tallies, failures)
}

fn check(file: &str) {
    let (tallies, failures) = run(file);
    let mut total = Tally::default();
    println!("{file}");
    println!("  {:<12} {:>7} {:>10} {:>7} {:>9}", "grammar", "cases", "mismatches", "throws", "text-skip");
    for (grammar, t) in &tallies {
        println!("  {:<12} {:>7} {:>10} {:>7} {:>9}", grammar, t.cases, t.mismatches, t.throws, t.text);
        total.cases += t.cases;
        total.mismatches += t.mismatches;
        total.throws += t.throws;
        total.text += t.text;
    }
    println!("  {:<12} {:>7} {:>10} {:>7} {:>9}", "total", total.cases, total.mismatches, total.throws, total.text);
    for f in &failures {
        println!("  MISMATCH {f}");
    }
    assert_eq!(total.mismatches, 0, "{file}: {} of {} cases differ from lowlight", total.mismatches, total.cases);
}

#[test]
fn corpus_matches_lowlight() {
    check("tests/vectors/corpus.tsv.gz");
}

#[test]
fn fuzz_matches_lowlight() {
    check("tests/vectors/fuzz.tsv.gz");
}

#[test]
fn full_fuzz_matches_lowlight() {
    let file = "tests/vectors/full/fuzz-full.tsv.gz";
    if !crate_dir().join(file).exists() {
        println!("{file} not present (node tools/vectors.mjs --full); skipped");
        return;
    }
    check(file);
}
