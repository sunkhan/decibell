//! The destination guard (port of `electron/main/publicNet.ts` plus `assertAllowed` in
//! `linkPreview.ts`). Requests that other people's message text drives (a link preview's page, its
//! redirects, its `og:image`, its oEmbed endpoint) must only ever reach the public internet: never
//! this machine, the LAN, the cloud metadata address or anything that tunnels to them.
//!
//! Three layers, all needed, applied to every hop (the first request and every redirect):
//!
//! 1. [`check_url`]: what the URL alone decides. `http:` / `https:` only; no userinfo; host names
//!    `localhost`, `*.local`, `*.internal`, `*.localdomain`, `*.home.arpa` (one trailing dot
//!    ignored) refused; an IP literal must be public. The URL is parsed with the WHATWG parser
//!    (`url` crate, as `new URL` in Electron), so every IPv4 spelling (`0x7f.1`, `2130706433`,
//!    `0177.0.0.1`, `127.1`, a trailing dot, full-width digits) is already canonical here, and an
//!    IPv6 literal is an address, never a string to pattern-match.
//! 2. [`resolve_public`]: a name is resolved once, and refused when it resolves to nothing or to
//!    any non-public address (one private answer is enough: a rebinding name mixes them).
//! 3. The connector (`http.rs`) connects only to the addresses step 2 vetted (never resolving the
//!    name again), so a rebinding name can't answer one address to the check and another to the
//!    connection. IP literals are never resolved: step 1 is their whole check.
//!
//! [`is_public_address`] classifies one address like Node's `net.BlockList` with the rules below:
//! an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`, whatever its spelling) is also checked against the
//! IPv4 rules, an IPv4 address against the IPv6 rules in its mapped form.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::Arc;

use url::{Host, Url};

/// IPv4 ranges refused (publicNet.ts `V4_BLOCKED`).
const V4_BLOCKED: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8),       // "this network"
    (Ipv4Addr::new(10, 0, 0, 0), 8),      // private
    (Ipv4Addr::new(100, 64, 0, 0), 10),   // CGNAT
    (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16),  // link-local (cloud metadata)
    (Ipv4Addr::new(172, 16, 0, 0), 12),   // private
    (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24),    // TEST-NET-1
    (Ipv4Addr::new(192, 88, 99, 0), 24),  // 6to4 relay anycast
    (Ipv4Addr::new(192, 168, 0, 0), 16),  // private
    (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // TEST-NET-2
    (Ipv4Addr::new(203, 0, 113, 0), 24),  // TEST-NET-3
    (Ipv4Addr::new(224, 0, 0, 0), 4),     // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved, broadcast
];

/// IPv6 ranges refused (publicNet.ts `V6_BLOCKED`). IPv4-mapped (`::ffff:0:0/96`) is deliberately
/// absent: mapped addresses are checked against the IPv4 rules (a rule here would block all IPv4).
const V6_BLOCKED: &[(Ipv6Addr, u8)] = &[
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 96), // ::/96 unspecified, loopback, IPv4-compatible
    (Ipv6Addr::new(0, 0, 0, 0, 0xffff, 0, 0, 0), 96), // ::ffff:0:0:0/96 IPv4-translated
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96), // 64:ff9b::/96 NAT64
    (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48), // 64:ff9b:1::/48 local-use NAT64
    (Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0), 64), // 100::/64 discard-only
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 32), // 2001::/32 Teredo
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32), // 2001:db8::/32 documentation
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16), // 2002::/16 6to4
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7), // fc00::/7 unique-local
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10), // fe80::/10 link-local
    (Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10), // fec0::/10 site-local (deprecated)
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8), // ff00::/8 multicast
];

fn in_v4(ip: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    u32::from(ip) & mask == u32::from(net) & mask
}

fn in_v6(ip: Ipv6Addr, net: Ipv6Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    };
    u128::from(ip) & mask == u128::from(net) & mask
}

/// The IPv4 address inside `::ffff:a.b.c.d` (IPv4-mapped only, not IPv4-compatible `::a.b.c.d`).
fn mapped_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    match ip.segments() {
        [0, 0, 0, 0, 0, 0xffff, hi, lo] => {
            Some(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)))
        }
        _ => None,
    }
}

fn blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            V4_BLOCKED.iter().any(|&(net, p)| in_v4(v4, net, p))
                || V6_BLOCKED
                    .iter()
                    .any(|&(net, p)| in_v6(v4.to_ipv6_mapped(), net, p))
        }
        IpAddr::V6(v6) => {
            V6_BLOCKED.iter().any(|&(net, p)| in_v6(v6, net, p))
                || mapped_v4(v6)
                    .is_some_and(|v4| V4_BLOCKED.iter().any(|&(net, p)| in_v4(v4, net, p)))
        }
    }
}

/// True only for an address outside every refused range (`isPublicAddress`). Takes a parsed
/// address: the strings the TS also refused (unparseable, or carrying an IPv6 zone id) can't
/// reach here — URL hosts come from the WHATWG parser and resolver answers are `IpAddr`s.
pub fn is_public_address(ip: IpAddr) -> bool {
    !blocked(ip)
}

/// Why a destination was refused. `Display` is the TS's error message for the same refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not `http:` / `https:`.
    Scheme,
    /// The URL carries a user name or password.
    Userinfo,
    /// No host, or a local-only name (`localhost`, `*.local`, `*.internal`, `*.localdomain`,
    /// `*.home.arpa`).
    Host,
    /// An IP literal outside the public internet.
    PrivateAddress,
    /// A name that resolved to nothing, or to at least one non-public address.
    NotPublic(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Scheme => f.write_str("scheme"),
            Refusal::Userinfo => f.write_str("userinfo"),
            Refusal::Host => f.write_str("host"),
            Refusal::PrivateAddress => f.write_str("private address"),
            Refusal::NotPublic(name) => write!(f, "{name} resolves to a non-public address"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Where [`check_url`] says a URL may connect: an IP literal (already checked) or a name still to
/// be resolved with [`resolve_public`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Ip(IpAddr),
    /// As the URL spells it (a trailing dot kept: that's what Node hands to `dns.lookup`).
    Name(String),
}

/// `/(^|\.)(localhost|local|internal|localdomain|home\.arpa)$/i`
fn blocked_name(host: &str) -> bool {
    const NAMES: &[&str] = &["localhost", "local", "internal", "localdomain", "home.arpa"];
    NAMES.iter().any(|name| {
        host.len() >= name.len() && {
            let (head, tail) = host.split_at(host.len() - name.len());
            tail.eq_ignore_ascii_case(name) && (head.is_empty() || head.ends_with('.'))
        }
    })
}

/// What can be decided from the URL alone (`assertAllowed`): scheme, userinfo, local-only names,
/// and the whole check for an IP literal.
pub fn check_url(u: &Url) -> Result<Destination, Refusal> {
    check_url_with(u, AddressRule::PublicOnly)
}

pub(crate) fn check_url_with(u: &Url, rule: AddressRule) -> Result<Destination, Refusal> {
    if u.scheme() != "http" && u.scheme() != "https" {
        return Err(Refusal::Scheme);
    }
    if !u.username().is_empty() || u.password().is_some_and(|p| !p.is_empty()) {
        return Err(Refusal::Userinfo);
    }
    match u.host() {
        None => Err(Refusal::Host),
        Some(Host::Domain(d)) => {
            // `hostname.replace(/^\[|\]$/g, "").replace(/\.$/, "")`: brackets only ever wrap IPv6.
            let h = d.strip_suffix('.').unwrap_or(d);
            if h.is_empty() || blocked_name(h) {
                Err(Refusal::Host)
            } else {
                Ok(Destination::Name(d.to_owned()))
            }
        }
        Some(Host::Ipv4(a)) => rule
            .check(IpAddr::V4(a))
            .map(|()| Destination::Ip(IpAddr::V4(a))),
        Some(Host::Ipv6(a)) => rule
            .check(IpAddr::V6(a))
            .map(|()| Destination::Ip(IpAddr::V6(a))),
    }
}

/// Which addresses a connection may reach. Production code has exactly one rule; the loopback
/// relaxation exists only in this crate's own test build and with the `loopback` feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressRule {
    PublicOnly,
    /// Public, or IPv4 loopback (127.0.0.0/8): the in-process test servers. Test builds and the
    /// `loopback` feature (the client's self-tests against a local fixture server) only.
    #[cfg(any(test, feature = "loopback"))]
    PublicOrLoopback,
}

impl AddressRule {
    pub(crate) fn allows(self, ip: IpAddr) -> bool {
        match self {
            AddressRule::PublicOnly => is_public_address(ip),
            #[cfg(any(test, feature = "loopback"))]
            AddressRule::PublicOrLoopback => {
                is_public_address(ip) || matches!(ip, IpAddr::V4(v4) if v4.is_loopback())
            }
        }
    }

    fn check(self, ip: IpAddr) -> Result<(), Refusal> {
        if self.allows(ip) {
            Ok(())
        } else {
            Err(Refusal::PrivateAddress)
        }
    }
}

/// A DNS resolver: the system's (`getaddrinfo`, as Node's `dns.lookup`) in production, a scripted
/// one in tests.
pub(crate) trait Resolve: Send + Sync + 'static {
    fn resolve(&self, name: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;
}

/// `getaddrinfo` on tokio's blocking pool, every address in the order the system returns them.
pub(crate) struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        let name = name.to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    }
}

/// Resolve `name` once and refuse it unless every answer is allowed (`publicOnlyLookup`). The
/// returned addresses are the only ones the connection may use.
pub(crate) async fn resolve_public(
    name: &str,
    resolver: &Arc<dyn Resolve>,
    rule: AddressRule,
) -> Result<Vec<IpAddr>, crate::Error> {
    let list = resolver
        .resolve(name)
        .await
        .map_err(|e| crate::Error::Network(Box::new(e)))?;
    if list.is_empty() || list.iter().any(|&ip| !rule.allows(ip)) {
        return Err(crate::Error::Refused(Refusal::NotPublic(name.to_owned())));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn every_v4_range_is_refused_at_both_ends() {
        for &(net, prefix) in V4_BLOCKED {
            let first = u32::from(net);
            let last = first | (u32::MAX >> prefix);
            for a in [first, last, first + (last - first) / 2] {
                let v4 = Ipv4Addr::from(a);
                assert!(!is_public_address(IpAddr::V4(v4)), "{v4} in {net}/{prefix}");
                // …and in every IPv4-mapped spelling (they're one address once parsed).
                assert!(
                    !is_public_address(IpAddr::V6(v4.to_ipv6_mapped())),
                    "::ffff:{v4}"
                );
            }
            // Just outside the range.
            if let Some(below) = first.checked_sub(1) {
                let v4 = Ipv4Addr::from(below);
                if !V4_BLOCKED.iter().any(|&(n, p)| in_v4(v4, n, p)) {
                    assert!(
                        is_public_address(IpAddr::V4(v4)),
                        "{v4} just below {net}/{prefix}"
                    );
                }
            }
            if let Some(above) = last.checked_add(1) {
                let v4 = Ipv4Addr::from(above);
                if !V4_BLOCKED.iter().any(|&(n, p)| in_v4(v4, n, p)) {
                    assert!(
                        is_public_address(IpAddr::V4(v4)),
                        "{v4} just above {net}/{prefix}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_v6_range_is_refused_at_both_ends() {
        for &(net, prefix) in V6_BLOCKED {
            let first = u128::from(net);
            let last = first | (u128::MAX >> prefix);
            for a in [first, last, first + (last - first) / 2] {
                let v6 = Ipv6Addr::from(a);
                assert!(!is_public_address(IpAddr::V6(v6)), "{v6} in {net}/{prefix}");
            }
        }
    }

    #[test]
    fn named_addresses() {
        for s in [
            "127.0.0.1",
            "127.255.255.254",
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::7f00:1",
            "::127.0.0.1",
            "::8.8.8.8",
            "::ffff:127.0.0.1",
            "::ffff:7f00:1",
            "::ffff:a9fe:a9fe",
            "::ffff:10.0.0.1",
            "::ffff:0:127.0.0.1",
            "64:ff9b::8.8.8.8",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:db8::1",
            "2002:7f00:1::",
            "2002:808:808::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
        ] {
            assert!(!is_public_address(ip(s)), "{s} must be refused");
        }
        for s in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "169.253.255.255",
            "192.0.1.1",
            "192.88.98.255",
            "192.169.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
            "11.0.0.0",
            "9.255.255.255",
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "::ffff:8.8.8.8",
            "2003::1",
            "2001:1::1",
            "2001:db7:ffff::1",
            "2001:db9::1",
            "fbff::1",
            "fe7f::1",
            "feff::1",
        ] {
            // fe80::/10 + fec0::/10 cover fe80..feff; fe7f is the first public value below them.
            if s == "feff::1" {
                assert!(!is_public_address(ip(s)), "{s} is site-local");
                continue;
            }
            assert!(is_public_address(ip(s)), "{s} must be allowed");
        }
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn url_checks() {
        use Refusal::*;
        let cases: &[(&str, Result<Destination, Refusal>)] = &[
            (
                "https://example.com/",
                Ok(Destination::Name("example.com".into())),
            ),
            (
                "http://example.com./x",
                Ok(Destination::Name("example.com.".into())),
            ),
            ("ftp://example.com/", Err(Scheme)),
            ("file:///etc/passwd", Err(Scheme)),
            ("data:text/html,hi", Err(Scheme)),
            ("javascript:alert(1)", Err(Scheme)),
            ("https://user@example.com/", Err(Userinfo)),
            ("https://:pw@example.com/", Err(Userinfo)),
            ("https://user:pw@example.com/", Err(Userinfo)),
            ("http://localhost/", Err(Host)),
            ("http://LOCALHOST./", Err(Host)),
            ("http://a.b.localhost/", Err(Host)),
            ("http://printer.local/", Err(Host)),
            ("http://metadata.google.internal/", Err(Host)),
            ("http://box.localdomain/", Err(Host)),
            ("http://nas.home.arpa/", Err(Host)),
            ("http://ｌｏｃａｌｈｏｓｔ/", Err(Host)), // IDNA maps full-width letters first
            (
                "http://notlocalhost/",
                Ok(Destination::Name("notlocalhost".into())),
            ),
            (
                "http://localhost.example.com/",
                Ok(Destination::Name("localhost.example.com".into())),
            ),
            // Two trailing dots: only one is stripped, so the name rule misses it — the resolver
            // check is what stops it (see resolver tests).
            (
                "http://localhost../",
                Ok(Destination::Name("localhost..".into())),
            ),
            ("http://127.0.0.1/", Err(PrivateAddress)),
            ("http://127.1/", Err(PrivateAddress)),
            ("http://0x7f.1/", Err(PrivateAddress)),
            ("http://0x7f000001/", Err(PrivateAddress)),
            ("http://2130706433/", Err(PrivateAddress)),
            ("http://017700000001/", Err(PrivateAddress)),
            ("http://0177.0.0.1/", Err(PrivateAddress)),
            ("http://127.0.0.1./", Err(PrivateAddress)),
            ("http://１２７.０.０.１/", Err(PrivateAddress)),
            ("http://0/", Err(PrivateAddress)),
            (
                "http://169.254.169.254/latest/meta-data/",
                Err(PrivateAddress),
            ),
            ("http://[::1]/", Err(PrivateAddress)),
            ("http://[::]/", Err(PrivateAddress)),
            ("http://[::ffff:127.0.0.1]/", Err(PrivateAddress)),
            ("http://[::ffff:7f00:1]/", Err(PrivateAddress)),
            ("http://[0:0:0:0:0:ffff:127.0.0.1]/", Err(PrivateAddress)),
            ("http://[::7f00:1]/", Err(PrivateAddress)),
            ("http://[::ffff:a9fe:a9fe]/", Err(PrivateAddress)),
            ("http://[64:ff9b::7f00:1]/", Err(PrivateAddress)),
            ("http://[2002:7f00:1::]/", Err(PrivateAddress)),
            ("http://[fe80::1]/", Err(PrivateAddress)),
            ("http://[fd00::1]/", Err(PrivateAddress)),
            ("http://8.8.8.8:22/", Ok(Destination::Ip(ip("8.8.8.8")))),
            (
                "http://[2606:4700:4700::1111]/",
                Ok(Destination::Ip(ip("2606:4700:4700::1111"))),
            ),
            (
                "http://[::ffff:8.8.8.8]/",
                Ok(Destination::Ip(ip("::ffff:8.8.8.8"))),
            ),
        ];
        for (s, want) in cases {
            assert_eq!(&check_url(&url(s)), want, "{s}");
        }
        // Zone ids and malformed literals never parse, so they never reach a connection.
        assert!(Url::parse("http://[fe80::1%25eth0]/").is_err());
        assert!(Url::parse("http://[::1/").is_err());
        assert!(Url::parse("http://1.2.3.4.5/").is_err());
    }

    struct Scripted(Vec<IpAddr>);
    impl Resolve for Scripted {
        fn resolve(
            &self,
            _: &str,
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
            let list = self.0.clone();
            Box::pin(async move { Ok(list) })
        }
    }

    async fn check(list: Vec<&'static str>) -> Result<Vec<IpAddr>, crate::Error> {
        let r: Arc<dyn Resolve> = Arc::new(Scripted(list.into_iter().map(ip).collect()));
        resolve_public("evil.example", &r, AddressRule::PublicOnly).await
    }

    #[tokio::test]
    async fn names_resolving_to_private_addresses_are_refused() {
        assert!(check(vec!["93.184.216.34"]).await.is_ok());
        assert!(check(vec!["93.184.216.34", "2606:4700::1"]).await.is_ok());
        for bad in [
            vec![],
            vec!["127.0.0.1"],
            vec!["93.184.216.34", "10.0.0.1"], // one private answer is enough
            vec!["2606:4700::1", "::1"],
            vec!["::ffff:192.168.0.1"],
            vec!["169.254.169.254"],
            vec!["0.0.0.0"], // what DNS sinkholes answer
        ] {
            let e = check(bad.clone()).await.unwrap_err();
            assert_eq!(
                e.to_string(),
                "evil.example resolves to a non-public address",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn loopback_relaxation_is_narrow() {
        let r = AddressRule::PublicOrLoopback;
        assert!(r.allows(ip("127.0.0.1")));
        assert!(r.allows(ip("127.9.9.9")));
        assert!(!r.allows(ip("::1")));
        assert!(!r.allows(ip("10.0.0.1")));
        assert!(!r.allows(ip("169.254.169.254")));
        assert!(!r.allows(ip("::ffff:127.0.0.1")));
    }
}
