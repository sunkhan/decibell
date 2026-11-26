//! Live smoke tests over the real internet, through the public API only. Ignored by default (the
//! GIF one spends a request of the key's hourly quota):
//!
//!     cargo test -p decibell-unfurl --test live -- --ignored
//!
//! Each skips (passes) when offline or, for GIFs, when no key is configured. Nothing here prints
//! the key; the request URL that carries it is never part of an error.

use std::net::ToSocketAddrs;
use std::path::Path;

use decibell_unfurl::{GifConfig, Gifs, LinkPreview, LinkPreviews};

fn online(host: &str) -> bool {
    (host, 443)
        .to_socket_addrs()
        .is_ok_and(|mut a| a.next().is_some())
}

#[tokio::test]
#[ignore = "network"]
async fn unfurls_a_public_page_over_real_tls() {
    if !online("example.com") {
        eprintln!("offline: skipped");
        return;
    }
    let previews = LinkPreviews::new("0.0.0-live");
    match previews.fetch("https://example.com/").await {
        Some(LinkPreview::Site(s)) => {
            assert_eq!(s.title.as_deref(), Some("Example Domain"));
            assert_eq!(s.site_name.as_deref(), Some("example.com"));
            eprintln!("{s:?}");
        }
        other => panic!("expected a site preview, got {other:?}"),
    }
    // The production guard: loopback and a name that only resolves locally are refused.
    assert!(previews.fetch("http://127.0.0.1/").await.is_none());
    assert!(previews.fetch("http://localhost/").await.is_none());
}

#[tokio::test]
#[ignore = "network; spends one request of the GIF key's quota"]
async fn klipy_or_giphy_search_for_cat() {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../electron-client/resources/gifs.json");
    let Some(config) = GifConfig::load(&[file]) else {
        eprintln!("no GIF key (gifs.json or GIF_API_KEY): skipped");
        return;
    };
    let host = match config.provider() {
        decibell_unfurl::GifProvider::Klipy => "api.klipy.com",
        decibell_unfurl::GifProvider::Giphy => "api.giphy.com",
    };
    if !online(host) {
        eprintln!("offline: skipped");
        return;
    }
    let provider = config.provider();
    let gifs = Gifs::new(Some(config));
    let page = match gifs.search("cat", None, Some("en-US"), false).await {
        Ok(p) => p,
        Err(e) => panic!("{} search failed: {e}", provider.label()),
    };
    assert!(!page.results.is_empty());
    for r in &page.results {
        assert!(r.url.starts_with("https://") && r.preview.starts_with("https://"));
        assert!(!r.id.is_empty());
    }
    eprintln!(
        "{}: {} results, next {:?}, first {:?} {}x{}",
        provider.label(),
        page.results.len(),
        page.next,
        page.results[0].title,
        page.results[0].width,
        page.results[0].height
    );
}
