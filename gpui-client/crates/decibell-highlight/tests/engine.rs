//! Engine pieces through the public API: language resolution, keywords,
//! relevance, sub-language detection, spans, budgets, and the checks that
//! tie the Rust side to the extracted data.

mod common;

use std::time::Duration;

use decibell_highlight::{
    highlight, highlight_language, highlight_with, testing, Budget, Fallback, Highlighted, Language, Node, Scope,
    Token,
};

fn texts<'a>(code: &'a str, out: &Highlighted, token: Token) -> Vec<&'a str> {
    out.spans.iter().filter(|s| s.token == token).map(|s| &code[s.range.clone()]).collect()
}

fn assert_tiles(code: &str, out: &Highlighted) {
    let mut at = 0;
    for span in &out.spans {
        assert_eq!(span.range.start, at, "spans are consecutive");
        assert!(span.range.end > span.range.start, "spans are not empty");
        assert!(code.is_char_boundary(span.range.end));
        at = span.range.end;
    }
    assert_eq!(at, code.len(), "spans cover the text");
}

#[test]
fn language_names_and_aliases() {
    assert_eq!(Language::all().len(), 27);
    for (name, expect) in [
        ("javascript", Language::Javascript),
        ("js", Language::Javascript),
        ("JS", Language::Javascript),
        ("JavaScript", Language::Javascript),
        ("jsx", Language::Javascript),
        ("ts", Language::Typescript),
        ("tsx", Language::Typescript),
        ("toml", Language::Ini),
        ("html", Language::Xml),
        ("svg", Language::Xml),
        ("sh", Language::Bash),
        ("c++", Language::Cpp),
        ("c#", Language::Csharp),
        ("yml", Language::Yaml),
        ("text", Language::Plaintext),
        ("md", Language::Markdown),
        ("docker", Language::Dockerfile),
        ("patch", Language::Diff),
    ] {
        assert_eq!(Language::from_name(name), Some(expect), "{name}");
    }
    for name in ["", "nosuchlang", "handlebars", "graphql", "java script", "js "] {
        assert_eq!(Language::from_name(name), None, "{name:?}");
    }
    assert_eq!(Language::Xml.name(), "xml");
    assert_eq!(Language::Xml.display_name(), "HTML, XML");
}

#[test]
fn unknown_or_missing_language_is_plain_text() {
    for lang in [None, Some("nosuchlang"), Some("")] {
        let code = "const a = 1; // not highlighted";
        let out = highlight(lang, code);
        assert_eq!(out.language, None);
        assert_eq!(out.fallback, None);
        assert_eq!(out.spans.len(), 1);
        assert_eq!(out.spans[0].range, 0..code.len());
        assert_eq!(out.spans[0].token, Token::Plain);
        assert!(out.path(&out.spans[0]).is_empty());
    }
    assert!(highlight(Some("js"), "").spans.is_empty());
    assert!(highlight(None, "").spans.is_empty());
}

#[test]
fn keywords_follow_the_language_case_rule() {
    // sql is case-insensitive: any spelling of a keyword is one
    let code = "SeLeCt a fRoM t wHeRe b IS NOT NULL";
    let out = highlight(Some("sql"), code);
    assert_tiles(code, &out);
    let kw = texts(code, &out, Token::Keyword);
    for word in ["SeLeCt", "fRoM", "wHeRe"] {
        assert!(kw.contains(&word), "{word} in {kw:?}");
    }
    // javascript is not
    let code = "const Const = CONST;";
    let out = highlight(Some("js"), code);
    assert_eq!(texts(code, &out, Token::Keyword), ["const"]);
}

#[test]
fn keyword_pattern_decides_what_a_word_is() {
    // css keywords include hyphens ($pattern /[a-z-]+/): `not` inside
    // `not-a-keyword` must not be picked out
    let code = "@media not all and (min-width: 1px) { a { color: red } }";
    let out = highlight(Some("css"), code);
    assert_tiles(code, &out);
    assert!(texts(code, &out, Token::Keyword).contains(&"not"));
    // powershell: -eq is one lexeme
    let code = "if ($a -eq 1) { Write-Host 'x' }";
    let out = highlight(Some("ps1"), code);
    assert_tiles(code, &out);
    assert!(texts(code, &out, Token::Keyword).iter().any(|k| k.contains("if")));
}

#[test]
fn relevance_matches_highlight_js() {
    // spot values; every vector row checks relevance as well
    let rel = |lang: Language, code: &str| {
        let units: Vec<u16> = code.encode_utf16().collect();
        testing::tree(lang, &units, &Budget::UNLIMITED).unwrap().1
    };
    assert_eq!(rel(Language::Plaintext, "anything"), 0.0);
    assert_eq!(rel(Language::Javascript, ""), 0.0);
    // a keyword counts at most MAX_KEYWORD_HITS (7) times
    let seven = rel(Language::Javascript, &"const ".repeat(7));
    assert_eq!(rel(Language::Javascript, &"const ".repeat(20)), seven);
    assert!(seven > rel(Language::Javascript, &"const ".repeat(6)));
    // common words carry no relevance
    assert_eq!(rel(Language::Javascript, "if in of for"), 0.0);
}

#[test]
fn html_picks_the_same_sub_languages_as_highlight_js() {
    let lang_of = |code: &str, needle: &str| -> Vec<Language> {
        let out = highlight(Some("html"), code);
        assert_tiles(code, &out);
        let at = code.find(needle).unwrap();
        let span = out.spans.iter().find(|s| s.range.contains(&at)).unwrap();
        out.path(span)
            .iter()
            .filter_map(|n| match n {
                Node::Language(l) => Some(*l),
                Node::Scope(_) => None,
            })
            .collect()
    };
    assert_eq!(lang_of("<style>a { color: red }</style>", "color"), [Language::Css]);
    assert_eq!(lang_of("<script>const a = () => 1;</script>", "const"), [Language::Javascript]);
    // nothing scores: plain text, no wrapper
    assert_eq!(lang_of("<script>zzz</script>", "zzz"), []);
    // markdown hands HTML to xml (an explicit sub-language)
    let code = "# t\n\n<div class=\"a\">\n";
    let out = highlight(Some("md"), code);
    let div = out.spans.iter().find(|s| &code[s.range.clone()] == "div").unwrap();
    assert_eq!(out.path(div)[0], Node::Language(Language::Xml));
    assert_eq!(div.token, Token::Title); // hljs-name
    // an unregistered sub-language (graphql) is left as text of the host
    let code = "const q = gql`query { a }`;";
    let out = highlight(Some("js"), code);
    assert!(out.spans.iter().all(|s| out.path(s).iter().all(|n| matches!(n, Node::Scope(_)))));
}

#[test]
fn spans_carry_colour_italic_and_path() {
    let code = "// TODO: fix\nfn main() { let s = \"x\"; }";
    let out = highlight(Some("rust"), code);
    assert_eq!(out.language, Some(Language::Rust));
    assert_tiles(code, &out);
    // a doctag in a comment: the keyword colour, and italic from the comment
    let doctag = out.spans.iter().find(|s| &code[s.range.clone()] == "TODO:").unwrap();
    assert_eq!(doctag.token, Token::Keyword);
    assert!(doctag.italic);
    assert_eq!(out.path(doctag), [Node::Scope(Scope::Comment), Node::Scope(Scope::Doctag)]);
    assert_eq!(out.scope(doctag), Some(Scope::Doctag));
    let comment = out.spans.iter().find(|s| code[s.range.clone()].starts_with("//")).unwrap();
    assert_eq!((comment.token, comment.italic), (Token::Comment, true));
    let main = out.spans.iter().find(|s| &code[s.range.clone()] == "main").unwrap();
    assert_eq!(main.token, Token::Title);
    assert_eq!(out.scope(main), Some(Scope::TitleFunction));
    assert!(!main.italic);
    assert_eq!(Scope::TitleFunction.css_classes(), "hljs-title function_");
    assert_eq!(Scope::TitleClassInherited.css_classes(), "hljs-title class_ inherited__");
    assert_eq!(Scope::Keyword.css_classes(), "hljs-keyword");
    assert_eq!(texts(code, &out, Token::String), ["\"x\""]);
}

#[test]
fn unstyled_scopes_inherit_the_enclosing_colour() {
    // `subst` (${…}) has no rule: its plain text shows the string's colour,
    // as CSS inheritance gives it in the renderer
    let code = "`a ${b} c`";
    let out = highlight(Some("js"), code);
    let b = out.spans.iter().find(|s| &code[s.range.clone()] == "${b}").expect("subst span");
    assert_eq!(out.scope(b), Some(Scope::Subst));
    assert_eq!(b.token, Token::String);
}

#[test]
fn byte_ranges_with_wide_characters() {
    let code = "let s = \"héllo 中 🎉\"; // 🎉 done";
    let out = highlight(Some("js"), code);
    assert_tiles(code, &out);
    assert_eq!(texts(code, &out, Token::String), ["\"héllo 中 🎉\""]);
    assert_eq!(texts(code, &out, Token::Comment), ["// 🎉 done"]);
}

#[test]
fn a_boundary_inside_a_surrogate_pair_snaps_to_the_character() {
    // JavaScript's `\\[\s\S]` takes one UTF-16 unit after the backslash, so
    // lowlight ends the escape between the two halves of the emoji. The
    // tree test covers the exact UTF-16 result; in byte ranges the boundary
    // moves to the start of the character and nothing is lost.
    let code = "x = \"a\\🎉b\";";
    let out = highlight(Some("js"), code);
    assert_tiles(code, &out);
}

#[test]
fn budget_fallbacks_are_plain_text() {
    let code = "const a = 1; // c\n".repeat(200);
    let ok = highlight(Some("js"), &code);
    assert_eq!(ok.fallback, None);
    assert!(ok.spans.len() > 200);

    let check = |budget: Budget, why: Fallback| {
        let out = highlight_with(Some("js"), &code, &budget);
        assert_eq!(out.language, Some(Language::Javascript));
        assert_eq!(out.fallback, Some(why));
        assert_eq!(out.spans.len(), 1);
        assert_eq!(out.spans[0].range, 0..code.len());
        assert_eq!(out.spans[0].token, Token::Plain);
        // the thread's budget is not left behind
        assert_eq!(testing::budget_remaining(), u64::MAX);
    };
    check(Budget { max_bytes: 100, ..Budget::default() }, Fallback::TooLong);
    check(Budget { max_steps: 500, ..Budget::default() }, Fallback::OutOfBudget);
    check(Budget { max_time: Some(Duration::ZERO), ..Budget::default() }, Fallback::OutOfBudget);
    // and the next highlight is unaffected
    assert_eq!(highlight(Some("js"), &code), ok);
}

#[test]
fn step_budget_is_deterministic_and_tight() {
    let code = "fn main() { println!(\"{}\", 1 + 2); }\n".repeat(50);
    let steps = testing::steps(Language::Rust, &code);
    assert!(steps > 0);
    assert_eq!(steps, testing::steps(Language::Rust, &code));
    let at = |max_steps| highlight_with(Some("rust"), &code, &Budget { max_steps, max_time: None, ..Budget::default() }).fallback;
    assert_eq!(at(steps), None);
    assert_eq!(at(steps - 1), Some(Fallback::OutOfBudget));
}

#[test]
fn budget_stops_a_sub_language_too() {
    let code = format!("<script>{}</script>", "let a = `x${1}`; ".repeat(400));
    let steps = testing::steps(Language::Xml, &code);
    let out = highlight_with(Some("html"), &code, &Budget { max_steps: steps / 2, max_time: None, ..Budget::default() });
    assert_eq!(out.fallback, Some(Fallback::OutOfBudget));
    assert_eq!(out.spans.len(), 1);
}

#[test]
fn every_grammar_regex_compiles() {
    let mut total = 0;
    for &language in Language::all() {
        total += testing::compile_all(language).unwrap_or_else(|e| panic!("{}: {e}", language.name()));
        assert!(language.is_loaded());
    }
    assert!(total > 900, "{total} regexes");
}

#[test]
fn joined_regexes_equal_highlight_js() {
    // tests/vectors/joined.txt: language, mode, first rule, and the source of
    // the alternation highlight.js itself built (_rewriteBackreferences)
    let text = std::fs::read_to_string(common::crate_dir().join("tests/vectors/joined.txt")).unwrap();
    let mut n = 0;
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(4, '\t').collect();
        let language = Language::from_name(f[0]).unwrap();
        let (mode, from): (u32, usize) = (f[1].parse().unwrap(), f[2].parse().unwrap());
        let expect = String::from_utf16(&common::json_string(f[3])).unwrap();
        assert_eq!(testing::joined_source(language, mode, from), expect, "{} mode {mode} from {from}", f[0]);
        n += 1;
    }
    assert!(n > 2000, "{n} joined regexes");
}

#[test]
fn colour_table_matches_the_renderer_stylesheet() {
    // every `.rich-codeblock .hljs-x { color: var(--y) }` of globals.css
    let css_path = common::crate_dir().join("../../../electron-client/src/styles/globals.css");
    let Ok(css) = std::fs::read_to_string(&css_path) else {
        println!("{} not found; skipped", css_path.display());
        return;
    };
    let mut seen = 0;
    let mut rest = css.as_str();
    while let Some(at) = rest.find(".rich-codeblock .hljs-") {
        // a rule: selectors up to `{`, declarations up to `}`
        let rule_start = at;
        let open = rule_start + rest[rule_start..].find('{').unwrap();
        let close = open + rest[open..].find('}').unwrap();
        let body = &rest[open + 1..close];
        let var = body.split("color: var(").nth(1).map(|s| &s[..s.find(')').unwrap()]).expect("a colour");
        for selector in rest[rule_start..open].split(',') {
            let class = selector.trim().strip_prefix(".rich-codeblock .hljs-").expect("selector shape");
            let token = Token::for_class(class).unwrap_or_else(|| panic!("hljs-{class} is styled by the renderer but has no Token"));
            assert_eq!(token.css_var(), var, "hljs-{class}");
            assert_eq!(body.contains("font-style: italic"), token == Token::Comment, "italic for hljs-{class}");
            seen += 1;
        }
        rest = &rest[close..];
    }
    assert_eq!(seen, 26, "styled classes in globals.css");
}

#[test]
fn every_styled_class_can_be_emitted_or_is_known_unused() {
    // classes the stylesheet styles that no registered grammar emits
    let unused: [&str; 0] = [];
    let mut heads: Vec<&str> = Vec::new();
    for &language in Language::all() {
        heads.extend(testing::scopes_used(language).iter().map(|s| s.head()));
    }
    for class in [
        "keyword", "built_in", "literal", "doctag", "meta", "title", "name", "section", "type", "selector-tag",
        "string", "regexp", "quote", "addition", "number", "symbol", "attr", "attribute", "variable",
        "template-variable", "selector-class", "selector-id", "comment", "deletion", "operator", "punctuation",
    ] {
        assert!(Token::for_class(class).is_some());
        assert_eq!(heads.contains(&class), !unused.contains(&class), "hljs-{class}");
    }
    // and the classes grammars emit that the stylesheet leaves unstyled
    // (they show the enclosing colour); listed so a new one is noticed
    let mut unstyled: Vec<&str> = heads.iter().copied().filter(|h| Token::for_class(h).is_none()).collect();
    unstyled.sort_unstable();
    unstyled.dedup();
    assert_eq!(
        unstyled,
        [
            "bullet", "char", "class", "code", "emphasis", "function", "link", "params", "property",
            "selector-attr", "selector-pseudo", "strong", "subst", "tag",
        ]
    );
}

#[test]
fn highlight_language_is_the_same_as_by_name() {
    let code = "a = 1";
    assert_eq!(highlight(Some("py"), code), highlight_language(Language::Python, code, &Budget::default()));
}
