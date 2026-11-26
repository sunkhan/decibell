//! The port against the TypeScript: every expectation here was produced by running the Electron
//! client's own modules (`tests/vectors/gen-vectors.mjs`) on the same inputs.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::Value;
use url::Url;

use super::server::{body_of, fixtures, hex, manifest};
use crate::gifs::test_access as g;
use crate::gifs::{GifConfig, GifPage, GifProvider};
use crate::link_preview::test_access as lp;
use crate::{links, public_net};

fn vectors(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn opt_s(v: &Value) -> Option<&str> {
    v.as_str()
}

#[test]
fn guard_classifies_addresses_like_blocklist() {
    let v = vectors("guard.json");
    let mut checked = 0;
    for c in v["isPublic"].as_array().unwrap() {
        let want = c["public"].as_bool().unwrap();
        match s(&c["ip"]).parse::<IpAddr>() {
            Ok(ip) => {
                assert_eq!(public_net::is_public_address(ip), want, "{}", c["ip"]);
                checked += 1;
            }
            // Strings Rust doesn't parse never reach the guard as addresses; Node refused them too.
            Err(_) => assert!(!want, "{} is public to Node but unparseable here", c["ip"]),
        }
    }
    assert!(checked > 90, "{checked}");
}

#[test]
fn guard_checks_urls_like_assert_allowed() {
    let v = vectors("guard.json");
    for c in v["assert"].as_array().unwrap() {
        let url = s(&c["url"]);
        let parsed = Url::parse(url);
        match opt_s(&c["href"]) {
            None => assert!(
                parsed.is_err(),
                "{url:?}: new URL threw, url crate parsed {parsed:?}"
            ),
            Some(href) => {
                let u =
                    parsed.unwrap_or_else(|e| panic!("{url:?}: new URL parsed, url crate: {e}"));
                assert_eq!(u.as_str(), href, "{url:?} href");
                // WHATWG hostname: brackets around IPv6, empty when there is no host.
                assert_eq!(
                    u.host_str().unwrap_or(""),
                    s(&c["hostname"]),
                    "{url:?} hostname"
                );
                let got = match public_net::check_url(&u) {
                    Ok(_) => "ok".to_owned(),
                    Err(r) => r.to_string(),
                };
                assert_eq!(got, s(&c["result"]), "{url:?}");
            }
        }
    }
    for c in v["joins"].as_array().unwrap() {
        let base = Url::parse(s(&c["base"])).unwrap();
        let got = base.join(s(&c["input"])).ok().map(String::from);
        assert_eq!(got.as_deref(), opt_s(&c["href"]), "join {:?}", c["input"]);
    }
}

#[test]
fn pages_decode_and_parse_like_the_ts() {
    let v = vectors("extract.json");
    let pages = v["pages"].as_array().unwrap();
    assert!(pages.len() >= 40);
    for c in pages {
        let file = s(&c["file"]);
        let bytes = std::fs::read(fixtures().join("html").join(file)).unwrap();
        let ct = s(&c["contentType"]);
        let decoded = lp::decode_html(&bytes, ct);
        assert_eq!(decoded, s(&c["decoded"]), "{file} as {ct}: decode");
        let want: lp::PageMeta = serde_json::from_value(c["page"].clone()).unwrap();
        assert_eq!(lp::parse_page(&decoded), want, "{file} as {ct}: parse");
    }
}

#[test]
fn text_helpers_match_the_ts() {
    let v = vectors("extract.json");
    for c in v["entities"].as_array().unwrap() {
        assert_eq!(
            lp::decode_entities(s(&c["in"])),
            s(&c["out"]),
            "entities {:?}",
            c["in"]
        );
    }
    for c in v["clean"].as_array().unwrap() {
        let max = c["max"].as_u64().unwrap() as usize;
        assert_eq!(
            lp::clean(s(&c["in"]), max),
            s(&c["out"]),
            "clean {:?} {max}",
            c["in"]
        );
    }
    for c in v["attrs"].as_array().unwrap() {
        let want: BTreeMap<String, String> = serde_json::from_value(c["out"].clone()).unwrap();
        assert_eq!(lp::parse_attrs(s(&c["in"])), want, "attrs {:?}", c["in"]);
    }
    for c in v["toInt"].as_array().unwrap() {
        assert_eq!(
            u64::from(lp::to_int(Some(s(&c["in"])))),
            c["out"].as_u64().unwrap(),
            "toInt {:?}",
            c["in"]
        );
    }
    for c in v["resolveImage"].as_array().unwrap() {
        let got = lp::resolve_image(Some(s(&c["raw"])), s(&c["base"]));
        assert_eq!(
            got.as_deref(),
            opt_s(&c["out"]),
            "resolveImage {:?}",
            c["raw"]
        );
    }
    for c in v["validColor"].as_array().unwrap() {
        assert_eq!(
            lp::valid_color(Some(s(&c["in"]))).as_deref(),
            opt_s(&c["out"]),
            "validColor {:?}",
            c["in"]
        );
    }
}

#[test]
fn image_probe_matches_the_ts() {
    let v = vectors("extract.json");
    let big = body_of(&manifest()["routes"]["/images/big-exif.jpg"]["body"]);
    for c in v["images"].as_array().unwrap() {
        let bytes = match c.get("generated").and_then(Value::as_str) {
            Some("big-exif") => big.clone(),
            Some("big-exif:65536") => big[..65536].to_vec(),
            Some(other) => panic!("unknown generator {other}"),
            None => hex(s(&c["hex"])),
        };
        let want = c["dims"].as_object().map(|d| {
            (
                d["width"].as_u64().unwrap() as u32,
                d["height"].as_u64().unwrap() as u32,
            )
        });
        assert_eq!(lp::probe_image_dims(&bytes), want, "{}", c["name"]);
    }
}

fn provider(v: &Value) -> GifProvider {
    serde_json::from_value(v.clone()).unwrap()
}

#[test]
fn gif_requests_and_answers_match_the_ts() {
    let v = vectors("gifs.json");
    let searches = v["searches"].as_array().unwrap();
    assert!(searches.len() > 50);
    for c in searches {
        let cfg = GifConfig::new(provider(&c["provider"]), s(&c["key"]));
        let (q, pos) = g::sanitize(s(&c["query"]), opt_s(&c["pos"]));
        let req = g::build_request(
            &cfg,
            &q,
            pos,
            opt_s(&c["locale"]),
            c["unfiltered"].as_bool().unwrap(),
        );
        assert_eq!(req.url.as_str(), s(&c["request"]["url"]), "request for {c}");
        let status = c["status"].as_u64().unwrap() as u16;
        let json = match c.get("answer").and_then(Value::as_str) {
            Some(f) => {
                serde_json::from_slice(&std::fs::read(fixtures().join("gifs").join(f)).unwrap())
                    .unwrap()
            }
            None => serde_json::from_str(s(&c["answerText"])).unwrap_or(Value::Null),
        };
        let got = match cfg.provider() {
            GifProvider::Klipy => g::parse_klipy(status, &json, req.cursor),
            GifProvider::Giphy => g::parse_giphy(status, &json, req.cursor),
        };
        let want = &c["result"];
        match got {
            Ok(page) => {
                assert_eq!(want["ok"], Value::Bool(true), "{c}: got {page:?}");
                let want: GifPage = serde_json::from_value(want["page"].clone()).unwrap();
                assert_eq!(
                    page,
                    want,
                    "{} {:?} {status}",
                    c["provider"],
                    c.get("answer")
                );
            }
            Err(e) => {
                assert_eq!(want["ok"], Value::Bool(false), "{c}: got error {e}");
                assert_eq!(
                    e.to_string(),
                    s(&want["error"]),
                    "{} {:?} {status}",
                    c["provider"],
                    c.get("answer")
                );
            }
        }
    }
}

#[test]
fn gif_config_loads_like_the_ts() {
    let v = vectors("gifs.json");
    let dir = std::env::temp_dir().join(format!("unfurl-gif-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("gifs.json");
    for c in v["configs"].as_array().unwrap() {
        match opt_s(&c["file"]) {
            Some(text) => std::fs::write(&file, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
        let got = g::config_from(
            opt_s(&c["envKey"]),
            opt_s(&c["envProvider"]),
            std::slice::from_ref(&file),
        );
        let status = &c["status"];
        assert_eq!(
            got.is_some(),
            status["configured"].as_bool().unwrap(),
            "{c}"
        );
        if let Some(cfg) = got {
            assert_eq!(cfg.provider(), provider(&status["provider"]), "{c}");
            assert_eq!(g::key(&cfg), s(&c["key"]), "{c}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gif_helpers_match_the_ts() {
    let v = vectors("gifs.json");
    for c in v["localeParts"].as_array().unwrap() {
        let (lang, region) = g::locale_parts(Some(s(&c["in"])));
        assert_eq!(lang.as_deref(), opt_s(&c["out"]["lang"]), "{c}");
        assert_eq!(region.as_deref(), opt_s(&c["out"]["region"]), "{c}");
    }
    for c in v["cursorInt"].as_array().unwrap() {
        let got = g::cursor_int(opt_s(&c["in"]), 5.0);
        assert_eq!(got, c["out"].as_f64().unwrap(), "{c}");
    }
}

#[test]
fn links_match_the_ts() {
    let v = vectors("links.json");
    for c in v["invites"].as_array().unwrap() {
        let url = s(&c["url"]);
        let want: Option<links::PendingInvite> =
            serde_json::from_value(c["parsed"].clone()).unwrap();
        assert_eq!(links::parse_invite_link(url), want, "{url:?}");
        assert_eq!(
            links::is_invite_link(url),
            c["isInvite"].as_bool().unwrap(),
            "{url:?}"
        );
    }
    for c in v["media"].as_array().unwrap() {
        let href = s(&c["href"]);
        assert_eq!(
            links::media_label_for(href).map(|l| l.as_str()),
            opt_s(&c["label"]),
            "{href:?}"
        );
        assert_eq!(
            links::is_gif_url(href),
            c["isGif"].as_bool().unwrap(),
            "{href:?}"
        );
    }
    assert_eq!(links::build_invite_link("XYZ123ABCD"), s(&v["build"]));
    let p = vectors("pipeline.json");
    for c in p["external"].as_array().unwrap() {
        assert_eq!(
            links::external_link_target(s(&c["url"])).as_deref(),
            opt_s(&c["opened"]),
            "{:?}",
            c["url"]
        );
    }
}
