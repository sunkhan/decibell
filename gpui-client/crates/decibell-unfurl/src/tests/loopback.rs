//! End to end over real sockets: the unfurler and the GIF client against the in-process fixture
//! server, through `HttpClient::for_tests` (127.0.0.0/8 allowed, scripted DNS, a test CA). Only
//! this test build can make such a client.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::io::Write;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::server::{self, fixtures, Reply};
use crate::gifs::test_access as g;
use crate::gifs::{GifConfig, GifProvider, Gifs};
use crate::http::HttpClient;
use crate::link_preview::test_access::Limits;
use crate::public_net::Resolve;
use crate::{Error, LinkPreview, LinkPreviews};

/// DNS by script: each name answers its list (the last one repeats), unknown names fail like
/// NXDOMAIN; every lookup is counted.
#[derive(Default)]
struct Dns {
    answers: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
    calls: Mutex<HashMap<String, usize>>,
}

impl Dns {
    fn with(entries: &[(&str, &[&[&str]])]) -> Arc<Self> {
        let d = Dns::default();
        for (name, seq) in entries {
            let seq = seq
                .iter()
                .map(|ips| ips.iter().map(|i| i.parse().unwrap()).collect())
                .collect();
            d.answers.lock().unwrap().insert((*name).to_owned(), seq);
        }
        Arc::new(d)
    }

    fn calls(&self, name: &str) -> usize {
        self.calls.lock().unwrap().get(name).copied().unwrap_or(0)
    }
}

impl Resolve for Dns {
    fn resolve(&self, name: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            let c = calls.entry(name.to_owned()).or_default();
            *c += 1;
            *c
        };
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get(name)
            .map(|seq| seq[(n - 1).min(seq.len() - 1)].clone());
        Box::pin(async move {
            answer.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "NXDOMAIN"))
        })
    }
}

fn client(pki: &server::TestPki, dns: Arc<Dns>) -> HttpClient {
    HttpClient::for_tests(dns, vec![pki.ca.clone()])
}

fn limits() -> Limits {
    Limits::ELECTRON
}

fn html(title: &str) -> Reply {
    Reply::Fixed {
        status: 200,
        headers: vec![("content-type".into(), b"text/html".to_vec())],
        body: format!("<html><head><title>{title}</title></head></html>").into_bytes(),
    }
}

fn title_of(p: &Option<LinkPreview>) -> Option<&str> {
    match p {
        Some(LinkPreview::Site(s)) => s.title.as_deref(),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_matches_the_ts() {
    let pki = server::pki();
    let srv = server::start(server::manifest_routes(), &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let expected: Value = serde_json::from_slice(
        &std::fs::read(fixtures().join("../vectors/pipeline.json")).unwrap(),
    )
    .unwrap();
    let cases = expected["cases"].as_array().unwrap();
    assert!(cases.len() >= 100);
    for c in cases {
        let url = srv.subst(c["url"].as_str().unwrap());
        let got = serde_json::to_value(previews.fetch(&url).await).unwrap();
        let want: Value = serde_json::from_str(&srv.subst(&c["preview"].to_string())).unwrap();
        assert_eq!(got, want, "{}", c["url"]);
    }
}

#[tokio::test]
async fn sends_electrons_request_headers() {
    let pki = server::pki();
    let srv = server::start(server::manifest_routes(), &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    assert!(previews
        .fetch(&format!("{}/og-full.html", srv.origin))
        .await
        .is_some());
    let reqs = srv.requests.lock().unwrap().clone();
    let (_, headers) = reqs.iter().find(|(p, _)| p == "/og-full.html").unwrap();
    let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "Host",
            "User-Agent",
            "Accept",
            "Accept-Language",
            "Accept-Encoding"
        ]
    );
    let h: HashMap<&str, &str> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(h["Host"], srv.hostport);
    assert_eq!(
        h["User-Agent"],
        "Mozilla/5.0 (compatible; Decibell/0.0.0-test; +https://github.com/sunkhan/decibell)"
    );
    assert_eq!(
        h["Accept"],
        "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5"
    );
    assert_eq!(h["Accept-Language"], "en,*;q=0.5");
    assert_eq!(h["Accept-Encoding"], "gzip, deflate, br");
    // Declared image dimensions: no probe.
    assert_eq!(srv.hits("/static/cover.png"), 0);
}

#[tokio::test]
async fn https_by_ip_and_by_name() {
    let pki = server::pki();
    let srv = server::start(server::manifest_routes(), &pki).await;
    let dns = Dns::with(&[("a.test", &[&["127.0.0.1"]]), ("b.test", &[&["127.0.0.1"]])]);
    let previews = LinkPreviews::for_tests(client(&pki, dns), limits());
    // IP literal: no SNI, the certificate's IP SAN.
    let by_ip = previews
        .fetch(&format!("https://127.0.0.1:{}/og-full.html", srv.port))
        .await;
    assert_eq!(title_of(&by_ip), Some("The Full Story"));
    // A name: SNI + DNS SAN.
    let by_name = previews
        .fetch(&format!("https://a.test:{}/og-full.html", srv.port))
        .await;
    assert_eq!(title_of(&by_name), Some("The Full Story"));
    // A name the certificate doesn't cover: refused by TLS.
    assert!(previews
        .fetch(&format!("https://b.test:{}/og-full.html", srv.port))
        .await
        .is_none());
}

#[tokio::test]
async fn names_are_resolved_once_and_every_answer_checked() {
    let pki = server::pki();
    let mut routes = server::manifest_routes();
    routes.insert(
        "/to-evil".into(),
        Reply::Fixed {
            status: 302,
            headers: vec![(
                "location".into(),
                b"http://evil.test:{port}/og-full.html".to_vec(),
            )],
            body: vec![],
        },
    );
    routes.insert(
        "/to-good".into(),
        Reply::Fixed {
            status: 302,
            headers: vec![(
                "location".into(),
                b"http://good.test:{port}/title-only.html".to_vec(),
            )],
            body: vec![],
        },
    );
    let srv = server::start(routes, &pki).await;
    let dns = Dns::with(&[
        // A rebinding name: loopback (allowed in this test build) to the check, private after.
        ("rebind.test", &[&["127.0.0.1"], &["10.0.0.1"]]),
        ("mixed.test", &[&["127.0.0.1", "10.0.0.1"]]),
        ("mapped.test", &[&["::ffff:192.168.0.1"]]),
        ("evil.test", &[&["10.0.0.5"]]),
        ("good.test", &[&["127.0.0.1"]]),
        ("localhost", &[&["127.0.0.1"]]),
    ]);
    let previews = LinkPreviews::for_tests(client(&pki, dns.clone()), limits());
    let p = srv.port;

    let r = previews
        .fetch(&format!("http://rebind.test:{p}/og-full.html"))
        .await;
    assert_eq!(
        title_of(&r),
        Some("The Full Story"),
        "connected to the vetted address"
    );
    assert_eq!(
        dns.calls("rebind.test"),
        1,
        "one lookup per hop; the connection never resolves again"
    );

    let before = srv.hits("/og-full.html");
    assert!(previews
        .fetch(&format!("http://mixed.test:{p}/og-full.html?m"))
        .await
        .is_none());
    assert!(previews
        .fetch(&format!("http://mapped.test:{p}/og-full.html"))
        .await
        .is_none());
    assert!(previews
        .fetch(&format!("http://localhost:{p}/og-full.html"))
        .await
        .is_none());
    assert_eq!(
        dns.calls("localhost"),
        0,
        "refused by name before any lookup"
    );
    // Every hop is checked: a redirect to a name that resolves privately goes nowhere.
    assert!(previews
        .fetch(&format!("{}/to-evil", srv.origin))
        .await
        .is_none());
    assert_eq!(dns.calls("evil.test"), 1);
    assert_eq!(
        srv.hits("/og-full.html"),
        before,
        "no refused destination reached the server"
    );
    // …while one resolving publicly (loopback, in this build) is followed.
    let r = previews.fetch(&format!("{}/to-good", srv.origin)).await;
    assert_eq!(title_of(&r), Some("Plain <title> & a description"));
}

#[tokio::test]
async fn reads_stop_at_the_caps() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    // 32 MiB of head with no </head>: read to 3 MiB, then the connection is dropped.
    let mut huge = b"<html><head><title>Huge</title><!--".to_vec();
    huge.resize(32 << 20, b'x');
    routes.insert(
        "/huge".into(),
        Reply::Trickle {
            headers: vec![("content-type".into(), b"text/html".to_vec())],
            body: huge,
            chunk: 64 << 10,
            delay: Duration::ZERO,
            repeat: false,
        },
    );
    // A gzip bomb: 64 MiB of zeros behind a title, ~64 KiB on the wire.
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(b"<html><head><title>Bomb</title>").unwrap();
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..64 {
        gz.write_all(&zeros).unwrap();
    }
    routes.insert(
        "/bomb".into(),
        Reply::Fixed {
            status: 200,
            headers: vec![
                ("content-type".into(), b"text/html".to_vec()),
                ("content-encoding".into(), b"gzip".to_vec()),
            ],
            body: gz.finish().unwrap(),
        },
    );
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let t = Instant::now();
    assert_eq!(
        title_of(&previews.fetch(&format!("{}/huge", srv.origin)).await),
        Some("Huge")
    );
    assert!(
        srv.sent("/huge") < 16 << 20,
        "stopped reading: {} bytes sent",
        srv.sent("/huge")
    );
    assert_eq!(
        title_of(&previews.fetch(&format!("{}/bomb", srv.origin)).await),
        Some("Bomb")
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[tokio::test]
async fn bodies_that_cannot_matter_are_not_read() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    routes.insert(
        "/video.mp4".into(),
        Reply::Trickle {
            headers: vec![("content-type".into(), b"video/mp4".to_vec())],
            body: vec![0; 8 << 20],
            chunk: 64 << 10,
            delay: Duration::from_millis(5),
            repeat: false,
        },
    );
    // A GIF whose header arrives at once, then bytes forever.
    let mut gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
    gif.resize(64 << 10, 0);
    routes.insert(
        "/endless.gif".into(),
        Reply::Trickle {
            headers: vec![("content-type".into(), b"image/gif".to_vec())],
            body: gif,
            chunk: 16 << 10,
            delay: Duration::from_millis(20),
            repeat: true,
        },
    );
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let t = Instant::now();
    assert!(previews
        .fetch(&format!("{}/video.mp4", srv.origin))
        .await
        .is_none());
    let img = previews.fetch(&format!("{}/endless.gif", srv.origin)).await;
    match img {
        Some(LinkPreview::Image(i)) => assert_eq!((i.width, i.height, i.gif), (320, 240, true)),
        other => panic!("{other:?}"),
    }
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert!(
        srv.sent("/video.mp4") < 2 << 20,
        "{}",
        srv.sent("/video.mp4")
    );
    assert!(
        srv.sent("/endless.gif") < 2 << 20,
        "{}",
        srv.sent("/endless.gif")
    );
}

#[tokio::test]
async fn each_request_has_a_deadline() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    routes.insert("/never".into(), Reply::Never);
    routes.insert(
        "/stall".into(),
        Reply::Trickle {
            headers: vec![("content-type".into(), b"text/html".to_vec())],
            body: b"<html><head><title>Stalled".to_vec(),
            chunk: 8,
            delay: Duration::from_secs(3600),
            repeat: false,
        },
    );
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(
        client(&pki, Dns::with(&[])),
        Limits {
            timeout: Duration::from_millis(300),
            ..limits()
        },
    );
    let t = Instant::now();
    assert!(previews
        .fetch(&format!("{}/never", srv.origin))
        .await
        .is_none());
    assert!(previews
        .fetch(&format!("{}/stall", srv.origin))
        .await
        .is_none());
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[tokio::test]
async fn results_are_cached_and_expire_on_access() {
    let pki = server::pki();
    let srv = server::start(server::manifest_routes(), &pki).await;
    let ttl = Duration::from_millis(300);
    let previews = LinkPreviews::for_tests(
        client(&pki, Dns::with(&[])),
        Limits {
            ok_ttl: ttl,
            fail_ttl: ttl,
            ..limits()
        },
    );
    let ok = format!("{}/title-only.html", srv.origin);
    let miss = format!("{}/status/404", srv.origin);
    assert!(previews.fetch(&ok).await.is_some());
    assert!(previews.fetch(&ok).await.is_some());
    assert!(previews.fetch(&miss).await.is_none());
    assert!(previews.fetch(&miss).await.is_none());
    assert_eq!(
        (srv.hits("/title-only.html"), srv.hits("/status/404")),
        (1, 1),
        "hits and misses cached"
    );
    tokio::time::sleep(ttl + Duration::from_millis(100)).await;
    assert!(previews.fetch(&ok).await.is_some());
    assert!(previews.fetch(&miss).await.is_none());
    assert_eq!(
        (srv.hits("/title-only.html"), srv.hits("/status/404")),
        (2, 2),
        "expired entries refetched"
    );
}

#[tokio::test]
async fn cache_drops_its_oldest_quarter_when_full() {
    let pki = server::pki();
    let srv = server::start(server::manifest_routes(), &pki).await;
    let previews = LinkPreviews::for_tests(
        client(&pki, Dns::with(&[])),
        Limits {
            cache_max: 8,
            ..limits()
        },
    );
    let url = |i: usize| format!("{}/title-only.html?{i}", srv.origin);
    for i in 0..8 {
        previews.fetch(&url(i)).await;
    }
    assert_eq!(previews.cached_len(), 8);
    previews.fetch(&url(8)).await;
    assert_eq!(previews.cached_len(), 7, "two oldest dropped, one added");
    let hits = srv.hits("/title-only.html");
    previews.fetch(&url(7)).await; // still cached
    assert_eq!(srv.hits("/title-only.html"), hits);
    previews.fetch(&url(0)).await; // evicted
    assert_eq!(srv.hits("/title-only.html"), hits + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_for_one_url_share_one_fetch() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    routes.insert(
        "/slow".into(),
        Reply::Delayed(Duration::from_millis(300), Box::new(html("Slow"))),
    );
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let url = format!("{}/slow", srv.origin);
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let (p, u) = (previews.clone(), url.clone());
            tokio::spawn(async move { p.fetch(&u).await })
        })
        .collect();
    for t in tasks {
        assert_eq!(title_of(&t.await.unwrap()), Some("Slow"));
    }
    assert_eq!(srv.hits("/slow"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_most_four_unfurls_run_at_once() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    for i in 0..10 {
        routes.insert(
            format!("/slow{i}"),
            Reply::Delayed(Duration::from_millis(200), Box::new(html("Slow"))),
        );
    }
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let tasks: Vec<_> = (0..10)
        .map(|i| {
            let (p, u) = (previews.clone(), format!("{}/slow{i}", srv.origin));
            tokio::spawn(async move { p.fetch(&u).await })
        })
        .collect();
    for t in tasks {
        assert!(t.await.unwrap().is_some());
    }
    assert_eq!(srv.max_active.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_caller_that_goes_away_still_fills_the_cache() {
    let pki = server::pki();
    let mut routes = HashMap::new();
    routes.insert(
        "/slow".into(),
        Reply::Delayed(Duration::from_millis(200), Box::new(html("Slow"))),
    );
    let srv = server::start(routes, &pki).await;
    let previews = LinkPreviews::for_tests(client(&pki, Dns::with(&[])), limits());
    let url = format!("{}/slow", srv.origin);
    let (p, u) = (previews.clone(), url.clone());
    let caller = tokio::spawn(async move { p.fetch(&u).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    caller.abort();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(title_of(&previews.fetch(&url).await), Some("Slow"));
    assert_eq!(srv.hits("/slow"), 1);
}

#[tokio::test]
async fn gif_search_end_to_end() {
    let pki = server::pki();
    let answer = std::fs::read(fixtures().join("gifs/klipy-search.json")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&answer).unwrap();
    let mut routes = HashMap::new();
    routes.insert(
        "/api/v1/test%20key/gifs/search".into(),
        Reply::Fixed {
            status: 200,
            headers: vec![
                ("content-type".into(), b"application/json".to_vec()),
                ("content-encoding".into(), b"gzip".to_vec()),
            ],
            body: gz.finish().unwrap(),
        },
    );
    routes.insert("/api/v1/test%20key/gifs/trending".into(), Reply::Never);
    let srv = server::start(routes, &pki).await;
    let gifs = Gifs::for_tests(
        GifConfig::new(GifProvider::Klipy, "test key"),
        client(&pki, Dns::with(&[])),
        &srv.origin,
        Duration::from_millis(300),
    );
    let page = gifs
        .search("  cat ", Some("2"), Some("en-US"), false)
        .await
        .unwrap();
    let want = g::parse_klipy(200, &serde_json::from_slice(&answer).unwrap(), 2.0).unwrap();
    assert_eq!(page, want);
    assert_eq!(page.next.as_deref(), Some("3"));
    let reqs = srv.requests.lock().unwrap().clone();
    let (_, headers) = &reqs[0];
    let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "host",
            "accept",
            "accept-language",
            "sec-fetch-mode",
            "user-agent",
            "accept-encoding"
        ]
    );
    let h: HashMap<&str, &str> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        (h["accept"], h["user-agent"], h["accept-encoding"]),
        ("application/json", "node", "gzip, deflate")
    );

    let e = gifs.search("", None, None, false).await.unwrap_err();
    assert!(matches!(e, Error::Timeout));
    assert_eq!(e.to_string(), "The operation was aborted due to timeout");

    // Nothing listening: "fetch failed", as undici says.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let gifs = Gifs::for_tests(
        GifConfig::new(GifProvider::Giphy, "k"),
        client(&pki, Dns::with(&[])),
        &dead,
        Duration::from_secs(2),
    );
    assert_eq!(
        gifs.search("cat", None, None, false)
            .await
            .unwrap_err()
            .to_string(),
        "fetch failed"
    );

    let none = Gifs::new(None);
    assert!(!none.status().configured);
    assert_eq!(
        none.search("cat", None, None, false)
            .await
            .unwrap_err()
            .to_string(),
        "not-configured"
    );
}

#[test]
fn production_client_refuses_loopback() {
    // The production constructor has no way to allow private addresses: the same fixture server
    // is unreachable through it.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pki = server::pki();
        let srv = server::start(server::manifest_routes(), &pki).await;
        let previews = LinkPreviews::new("0.0.0-test");
        assert!(previews
            .fetch(&format!("{}/og-full.html", srv.origin))
            .await
            .is_none());
        assert_eq!(srv.requests.lock().unwrap().len(), 0);
    });
}

#[tokio::test]
async fn remote_images_follow_redirects_within_the_guard_and_the_cap() {
    use crate::images::RemoteImages;
    let pki = server::pki();
    let mut routes = HashMap::new();
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5, 6];
    routes.insert(
        "/pic.png".to_string(),
        Reply::Fixed { status: 200, headers: vec![("content-type".into(), b"image/png".to_vec())], body: png.clone() },
    );
    routes.insert(
        "/moved".to_string(),
        Reply::Fixed { status: 302, headers: vec![("location".into(), b"/pic.png".to_vec())], body: vec![] },
    );
    // A redirect off https is refused (the CSP allowed `img-src https:` only).
    routes.insert(
        "/to-plain".to_string(),
        Reply::Fixed { status: 302, headers: vec![("location".into(), b"http://127.0.0.1:{port}/pic.png".to_vec())], body: vec![] },
    );
    routes.insert(
        "/big.gif".to_string(),
        Reply::Fixed { status: 200, headers: vec![("content-type".into(), b"image/gif".to_vec())], body: vec![7; 4096] },
    );
    routes.insert("/gone".to_string(), Reply::Fixed { status: 404, headers: vec![], body: b"nope".to_vec() });
    let srv = server::start(routes, &pki).await;
    let images = RemoteImages::for_tests(client(&pki, Dns::with(&[])), 1024);
    let at = |path: &str| format!("https://127.0.0.1:{}{path}", srv.port);
    assert_eq!(images.fetch(&at("/pic.png")).await.unwrap().as_ref(), png.as_slice());
    assert_eq!(images.fetch(&at("/moved")).await.unwrap().as_ref(), png.as_slice());
    assert!(matches!(images.fetch(&at("/to-plain")).await, Err(Error::InvalidUrl)));
    assert!(images.fetch(&at("/big.gif")).await.is_err());
    assert!(images.fetch(&at("/gone")).await.is_err());
    // Nothing is sent as a referrer.
    let requests = srv.requests.lock().unwrap();
    assert!(requests.iter().all(|(_, headers)| headers.iter().all(|(name, _)| !name.eq_ignore_ascii_case("referer"))));
    assert!(requests.iter().any(|(path, headers)| path == "/pic.png"
        && headers.iter().any(|(name, value)| name == "Accept" && value.starts_with("image/avif"))));
}
