//! Writes the fidelity corpus: the 10k-message fixture's paragraphs and a hand-written set,
//! as the neutral "segments" both sides lay out (`tools/README.md`).
//!
//!   cargo run -p decibell-text --example corpus -- --fixture texts.json --out corpus.json
//!
//! `texts.json` is a JSON array of message sources (`tools/fixture-texts.py` prints the
//! 10k fixture's). Every message is parsed by `decibell-richtext`; each paragraph block
//! becomes one case. Paragraphs with inline math or an emoji without a Twemoji asset are
//! left out (their boxes depend on fonts the harness page does not load).

use decibell_richtext::{Block, ParseOptions, SpanKind, parse};
use serde_json::{Value, json};

const BOLD: u32 = 1;
const ITALIC: u32 = 2;
const UNDERLINE: u32 = 4;
const STRIKE: u32 = 8;
const CODE: u32 = 16;
const LINK: u32 = 32;
const META: u32 = 64;

/// Hand-written sources (message grammar) for what the fixture does not reach.
const HAND: &[&str] = &[
    "Supercalifragilisticexpialidocious pneumonoultramicroscopicsilicovolcanoconiosis antidisestablishmentarianism floccinaucinihilipilification",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "see https://example.com/a/very/long/path/that/keeps/going/and/going?query=string&other=value#fragment-identifier for details",
    "https://sub.domain.example.org/path_with_underscores/and-dashes/file.name.ext?a=1&b=2&c=3&d=4&e=5&f=6&g=7&h=8",
    "trailing spaces here          and more words follow after the gap          end",
    "word    with    four    spaces    between    every    single    word    in    the    line    yes",
    "tab\tseparated\tvalues\tin\ta\trow\tof\tcells\tthat\twrap\taround",
    "a\tb\tc\n\tindented\n\t\ttwice",
    "soft\u{ad}hyphen\u{ad}ated extra\u{ad}ordinarily long\u{ad}ish com\u{ad}pound words in\u{ad}side a sen\u{ad}tence",
    "mixed **bold words** and *italic words* and __underlined words__ and ~~struck words~~ in one long line of text that wraps",
    "***bold italic*** then `inline code` then **bold `code in bold` bold** then *italic `code` italic*",
    "a chip at the very end of the line `cargo build --release --features local-central`",
    "`a-chip-at-the-start` followed by ordinary text that runs on for a while to wrap the line once or twice",
    "text `first chip` text `second chip with more words in it that wraps over the line edge for sure` text `third`",
    "`one very long chip without any spaces: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`",
    "😀 emoji at the start of a line and then text that goes on long enough to wrap around to the next",
    "text that ends with an emoji after enough words to come close to the right edge of the box 😀",
    "😀😃😄😁😆😅😂🤣😊😇🙂🙃😉😌😍🥰😘😗😙😚😋😛😝😜🤪🤨🧐🤓😎🥸🤩🥳😏😒😞😔😟😕🙁",
    "words 😀 between 😃 every 😄 pair 😁 of 😆 emoji 😅 in 😂 this 🤣 line 😊 here 😇 ok",
    "😀",
    "😀😃",
    "😀😃😄",
    "😀😃😄😁",
    "中文文本没有空格所以可以在任何两个字之间换行这是一个很长的句子用来测试换行行为是否与浏览器一致",
    "日本語のテキストもスペースなしで折り返されます。句読点「かぎかっこ」の禁則処理も確認します。",
    "한국어 텍스트는 공백이 있지만 단어 중간에서도 줄바꿈이 될 수 있습니다 확인해 봅시다",
    "mixed English and 中文 and 日本語 in one line with some more English words after the CJK text ends",
    "مرحبا بالعالم هذا نص عربي طويل لاختبار التفاف الاسطر في محرك النصوص الخاص بنا",
    "English then עברית טקסט בעברית then English again to see how the directions mix on one line",
    "punctuation!!! really??? yes... no---maybe (parenthesised text) [bracketed] {braced} \"quoted\" 'single'",
    "numbers 1,234,567.89 and 3.14159265358979323846 and 100% and $5.00 and #hashtag and @mention and a/b/c",
    "hyphen-ated-words-in-a-long-chain-of-hyphens-that-could-break-at-each-hyphen-if-allowed-to",
    "path/to/some/deeply/nested/directory/structure/with/many/components/file.txt and C:\\Windows\\System32\\drivers\\etc\\hosts",
    "line one\nline two\n\nline four after an empty line\nline five is long enough to wrap around the edge of the box at least once",
    "AV AVA To Ty Yo We LT PA VA WA AWAY TODAY Type AVATAR Tomorrow WAVE Yes",
    "Selam, naber? Güncelleme sonrası ses kanalı çalışıyor mu: ığüşöç İĞÜŞÖÇ",
    "Ελληνικά κείμενο και Русский текст для проверки других алфавитов в одной строке",
    "a b c d e f g h i j k l m n o p q r s t u v w x y z a b c d e f g h i j k l m n o p q r s t u v w x y z",
    "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
    "................................................................................................",
    "((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((",
    "word, word, word. word; word: word! word? word) word] word} (word [word {word \"word\" word",
    "x²+y²=z² — em dash – en dash … ellipsis ‘curly’ “quotes” • bullet © ® ™ ± × ÷ ≠ ≤ ≥ → ←",
    "ends with spaces     ",
    "     starts with spaces",
    "check this https://example.com/clips/123 and **this** and `that` 😂🔥",
];

fn main() {
    let mut fixture = None;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => fixture = args.next(),
            "--out" => out = args.next(),
            other => panic!("unknown argument {other}"),
        }
    }
    let out = out.expect("--out <corpus.json>");
    let texts: Vec<String> = match fixture {
        Some(path) => serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
        None => Vec::new(),
    };

    let styles = json!({
        // Nocturne / Matinee (the default look): Mona Sans.
        "n":   { "family": "Decibell Sans", "size": 14.5, "lh": 1.45, "ls_em": -0.01, "mono": "Decibell Mono", "meta": 11.5 },
        "n13": { "family": "Decibell Sans", "size": 13.0, "lh": 1.45, "ls_em": -0.01, "mono": "Decibell Mono", "meta": 10.31 },
        "n16": { "family": "Decibell Sans", "size": 16.0, "lh": 1.45, "ls_em": -0.01, "mono": "Decibell Mono", "meta": 12.69 },
        // Graphite: Inter.
        "g":   { "family": "Decibell Inter", "size": 13.0, "lh": 1.4, "ls_em": 0.0, "mono": "Decibell Mono", "meta": 10.0 },
    });

    let mut cases = Vec::new();
    let mut skipped = 0;
    for (index, text) in texts.iter().enumerate() {
        let edited = index % 25 == 7;
        for (block, segs) in paragraphs(text, edited).into_iter().enumerate() {
            let Some(segs) = segs else {
                skipped += 1;
                continue;
            };
            let id = format!("f{index}.{block}");
            cases.push(case(&id, "n", 560, "body", &segs));
            if index % 5 == 0 {
                for (style, width) in [("n", 320), ("n", 900), ("n13", 560), ("n16", 560), ("g", 560), ("g", 320)] {
                    cases.push(case(&id, style, width, "body", &segs));
                }
            }
        }
    }
    for (index, text) in HAND.iter().enumerate() {
        for (block, segs) in paragraphs(text, index % 4 == 1).into_iter().enumerate() {
            let Some(segs) = segs else { continue };
            let id = format!("h{index}.{block}");
            for style in ["n", "n13", "n16", "g"] {
                for width in [320, 560, 900] {
                    cases.push(case(&id, style, width, "body", &segs));
                }
            }
            // `white-space: normal` collapses white space; collapse it here, so that both
            // sides count offsets in the same text.
            let collapsed = collapse(&segs);
            for (wrap, segs) in [("words", &segs), ("normal", &collapsed)] {
                for width in [160, 320, 560] {
                    cases.push(case(&id, "n", width, wrap, segs));
                    cases.push(case(&id, "g", width, wrap, segs));
                }
            }
        }
    }
    eprintln!("{} cases ({} paragraphs skipped)", cases.len(), skipped);
    let corpus = json!({ "styles": styles, "cases": cases });
    std::fs::write(out, serde_json::to_string(&corpus).unwrap()).unwrap();
}

fn case(id: &str, style: &str, width: u32, wrap: &str, segs: &[Value]) -> Value {
    json!({ "id": format!("{id}/{style}/{width}/{wrap}"), "style": style, "w": width, "wrap": wrap, "segs": segs })
}

/// One entry per paragraph block: its segments, or `None` when it holds something the
/// harness cannot reproduce.
fn paragraphs(source: &str, edited: bool) -> Vec<Option<Vec<Value>>> {
    let document = parse(source, &ParseOptions::default());
    let emoji_size = document.jumbo_size().unwrap_or(decibell_richtext::INLINE_EMOJI_SIZE) as f64;
    let blocks: Vec<_> = document.blocks().collect();
    let last_paragraph = blocks.iter().rposition(|block| matches!(block, Block::Paragraph(_)));
    let mut out = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let Block::Paragraph(spans) = block else { continue };
        let mut segs = Vec::new();
        let mut usable = true;
        for span in spans.iter() {
            let text = document.text(source, span);
            let mut flags = 0;
            if span.style.is_bold() { flags |= BOLD }
            if span.style.is_italic() { flags |= ITALIC }
            if span.style.is_underline() { flags |= UNDERLINE }
            if span.style.is_strike() { flags |= STRIKE }
            if span.style.is_code() { flags |= CODE }
            match span.kind {
                SpanKind::Text => segs.push(json!([flags, text])),
                SpanKind::Link { .. } => segs.push(json!([flags | LINK, text])),
                SpanKind::Emoji { asset: Some(_) } => segs.push(json!(["box", emoji_size, emoji_size, 0.2])),
                _ => usable = false,
            }
        }
        if edited && Some(index) == last_paragraph && index + 1 == blocks.len() {
            segs.push(json!([META, "(edited)"]));
        }
        out.push(usable.then_some(segs));
    }
    out
}

fn collapse(segs: &[Value]) -> Vec<Value> {
    let mut after_space = true;
    let mut out = Vec::new();
    for seg in segs {
        let Some(text) = seg[1].as_str().filter(|_| seg[0].is_u64()) else {
            after_space = false;
            out.push(seg.clone());
            continue;
        };
        let mut collapsed = String::new();
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n') {
                if !after_space {
                    collapsed.push(' ');
                }
                after_space = true;
            } else {
                collapsed.push(c);
                after_space = false;
            }
        }
        if !collapsed.is_empty() {
            out.push(json!([seg[0], collapsed]));
        }
    }
    if let Some(last) = out.last_mut()
        && let Some(text) = last[1].as_str().filter(|text| text.ends_with(' '))
    {
        let trimmed = text.trim_end().to_string();
        last[1] = json!(trimmed);
    }
    out
}
