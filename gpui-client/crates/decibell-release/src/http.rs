//! A small blocking HTTP/1.1 client for the updater and the crash reporter: one connection per
//! request (`Connection: close`, nothing pooled, nothing left running), TLS by rustls with
//! Mozilla's roots (the store `decibell-unfurl` uses), and **only the hosts its owner names**: the
//! updater GitHub's release hosts, the crash reporter its DSN's host. Blocking on purpose: both
//! run on a thread that exists only while they work, and the crash reporter runs inside a panic
//! hook, where no async runtime can be trusted.
//!
//! Redirects are followed only to allowed hosts over HTTPS; bodies are length-delimited,
//! chunked or read to the end, never decompressed (`Accept-Encoding: identity`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use url::Url;

/// Why a request failed, in words the About tab can show after "Couldn't check: ".
#[derive(Debug)]
pub enum Error {
    /// A host or scheme this client may not contact.
    Refused(String),
    /// DNS, TCP, TLS or a read / write.
    Network(String),
    /// A status the caller didn't want.
    Status(u16),
    /// A malformed response.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(what) => write!(f, "refused to contact {what}"),
            Error::Network(what) => write!(f, "{what}"),
            Error::Status(status) => write!(f, "the server answered {status}"),
            Error::Protocol(what) => write!(f, "bad response: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Who may be contacted.
#[derive(Clone, Debug)]
pub struct Rule {
    /// HTTPS hosts (exact, lower-case).
    pub hosts: Vec<String>,
    /// Plain HTTP to 127.0.0.0/8 too: the self-tests' fixture servers. Never in a shipped build.
    #[cfg(any(test, feature = "test-hooks"))]
    pub loopback_http: bool,
}

impl Rule {
    pub fn hosts(hosts: &[&str]) -> Rule {
        Rule {
            hosts: hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
            #[cfg(any(test, feature = "test-hooks"))]
            loopback_http: false,
        }
    }

    /// Whether `url` may be contacted.
    pub fn allows(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else { return false };
        #[cfg(any(test, feature = "test-hooks"))]
        if self.loopback_http && url.scheme() == "http" {
            return host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback());
        }
        url.scheme() == "https" && self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
    }
}

pub struct Client {
    rule: Rule,
    user_agent: String,
    connect_timeout: Duration,
    io_timeout: Duration,
    #[cfg(test)]
    extra_root: Option<rustls::pki_types::CertificateDer<'static>>,
}

/// A response whose head is in; reading it reads the body.
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

impl Client {
    pub fn new(rule: Rule, user_agent: impl Into<String>) -> Client {
        Client {
            rule,
            user_agent: user_agent.into(),
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
            #[cfg(test)]
            extra_root: None,
        }
    }

    /// Shorter waits (the crash reporter's budget).
    pub fn timeouts(mut self, connect: Duration, io: Duration) -> Client {
        self.connect_timeout = connect;
        self.io_timeout = io;
        self
    }

    #[cfg(test)]
    pub(crate) fn trust(mut self, root: rustls::pki_types::CertificateDer<'static>) -> Client {
        self.extra_root = Some(root);
        self
    }

    /// GET, following up to five redirects (each to an allowed host). Returns the final URL too.
    pub fn get(&self, url: &Url, headers: &[(&str, &str)]) -> Result<(Url, Response), Error> {
        let mut url = url.clone();
        for _ in 0..=5 {
            let response = self.send("GET", &url, headers, None)?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .header("location")
                    .ok_or_else(|| Error::Protocol(format!("{} without a Location", response.status)))?;
                url = url.join(location).map_err(|e| Error::Protocol(format!("Location: {e}")))?;
                continue;
            }
            return Ok((url, response));
        }
        Err(Error::Protocol("too many redirects".into()))
    }

    /// POST `body` (no redirects).
    pub fn post(&self, url: &Url, headers: &[(&str, &str)], body: &[u8]) -> Result<Response, Error> {
        self.send("POST", url, headers, Some(body))
    }

    fn send(&self, method: &str, url: &Url, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<Response, Error> {
        if !self.rule.allows(url) {
            return Err(Error::Refused(url.host_str().unwrap_or("a URL without a host").to_string()));
        }
        let host = url.host_str().unwrap_or_default();
        let port = url.port_or_known_default().unwrap_or(443);
        let tcp = connect(host, port, self.connect_timeout)?;
        tcp.set_read_timeout(Some(self.io_timeout)).map_err(net)?;
        tcp.set_write_timeout(Some(self.io_timeout)).map_err(net)?;
        let mut stream = if url.scheme() == "https" {
            let name = ServerName::try_from(host.to_string()).map_err(|e| Error::Network(e.to_string()))?;
            let conn = rustls::ClientConnection::new(self.tls(), name).map_err(|e| Error::Network(e.to_string()))?;
            Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
        } else {
            Stream::Plain(tcp)
        };

        let target = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        let host_header = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        let mut head = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {}\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
            self.user_agent
        );
        for (name, value) in headers {
            if value.contains(['\r', '\n']) || name.contains(['\r', '\n', ':']) {
                return Err(Error::Protocol("a header with a line break".into()));
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if let Some(body) = body {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).map_err(net)?;
        if let Some(body) = body {
            stream.write_all(body).map_err(net)?;
        }
        stream.flush().map_err(net)?;

        let mut reader = BufReader::with_capacity(16 * 1024, stream);
        let (status, headers) = read_head(&mut reader)?;
        let find = |name: &str| headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
        let body = if method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status) {
            Body::Fixed(reader, 0)
        } else if find("transfer-encoding").is_some_and(|te| te.to_ascii_lowercase().contains("chunked")) {
            Body::Chunked { reader, left: 0, done: false }
        } else if let Some(length) = find("content-length") {
            let length = length.trim().parse().map_err(|_| Error::Protocol("Content-Length".into()))?;
            Body::Fixed(reader, length)
        } else {
            Body::Eof(reader)
        };
        Ok(Response { status, headers, body })
    }

    fn tls(&self) -> Arc<rustls::ClientConfig> {
        #[cfg(test)]
        if let Some(root) = &self.extra_root {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(root.clone()).expect("the test root");
            return Arc::new(config(roots));
        }
        static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
        CONFIG
            .get_or_init(|| {
                let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
                Arc::new(config(roots))
            })
            .clone()
    }
}

fn config(roots: rustls::RootCertStore) -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

fn net(e: io::Error) -> Error {
    Error::Network(e.to_string())
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, Error> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::Network(format!("couldn't resolve {host}: {e}")))?
        .collect();
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(Error::Network(match last {
        Some(e) => format!("couldn't reach {host}: {e}"),
        None => format!("{host} has no address"),
    }))
}

fn read_head<R: BufRead>(reader: &mut R) -> Result<(u16, Vec<(String, String)>), Error> {
    let mut raw = Vec::with_capacity(1024);
    loop {
        let before = raw.len();
        let n = reader.read_until(b'\n', &mut raw).map_err(net)?;
        if n == 0 {
            return Err(Error::Protocol("the connection closed before a response".into()));
        }
        if raw.len() > 64 * 1024 {
            return Err(Error::Protocol("response head over 64 KiB".into()));
        }
        // An empty line ends the head.
        if raw[before..] == *b"\r\n" || raw[before..] == *b"\n" {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut headers);
    match response.parse(&raw) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) => return Err(Error::Protocol("incomplete head".into())),
        Err(e) => return Err(Error::Protocol(e.to_string())),
    }
    let status = response.code.ok_or_else(|| Error::Protocol("no status".into()))?;
    let headers = response
        .headers
        .iter()
        .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).trim().to_string()))
        .collect();
    Ok((status, headers))
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The body's length when the server said it.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }

    /// The whole body, refused past `cap` bytes.
    pub fn read_capped(mut self, cap: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = self.read(&mut chunk).map_err(net)?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > cap {
                return Err(Error::Protocol(format!("body over {cap} bytes")));
            }
            out.extend_from_slice(&chunk[..n]);
        }
    }
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.body.read(buf)
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // A server that closes without close_notify: the body's own framing (length or
                // chunks) and the caller's checksum decide whether anything is missing.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                other => other,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

enum Body {
    /// `Content-Length`: this many bytes left.
    Fixed(BufReader<Stream>, u64),
    Chunked { reader: BufReader<Stream>, left: u64, done: bool },
    /// Neither: the body ends with the connection.
    Eof(BufReader<Stream>),
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Body::Fixed(reader, left) => {
                if *left == 0 || buf.is_empty() {
                    return Ok(0);
                }
                let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                let n = reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection closed mid-body"));
                }
                *left -= n as u64;
                Ok(n)
            }
            Body::Chunked { reader, left, done } => {
                if *done || buf.is_empty() {
                    return Ok(0);
                }
                if *left == 0 {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    let size = line.trim().split(';').next().unwrap_or("");
                    *left = u64::from_str_radix(size.trim(), 16)
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                    if *left == 0 {
                        // Trailers, then the empty line.
                        loop {
                            line.clear();
                            if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                                break;
                            }
                        }
                        *done = true;
                        return Ok(0);
                    }
                }
                let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                let n = reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection closed mid-chunk"));
                }
                *left -= n as u64;
                if *left == 0 {
                    let mut crlf = String::new();
                    reader.read_line(&mut crlf)?;
                }
                Ok(n)
            }
            Body::Eof(reader) => reader.read(buf),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A loopback server answering each connection with what `answer` returns for its request
    /// (head + body as one string), recording every request. Plain HTTP.
    pub(crate) struct Fixture {
        pub base: String,
        pub requests: Arc<Mutex<Vec<String>>>,
        #[allow(dead_code)]
        pub connections: Arc<AtomicUsize>,
    }

    pub(crate) fn serve(answer: impl Fn(&str, &[u8]) -> Vec<u8> + Send + Sync + 'static) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (r, c) = (requests.clone(), connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                c.fetch_add(1, Ordering::SeqCst);
                let (head, body) = read_request(&mut stream);
                r.lock().unwrap().push(head.clone());
                let reply = answer(&head, &body);
                let _ = stream.write_all(&reply);
            }
        });
        Fixture { base, requests, connections }
    }

    pub(crate) fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let length = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (head, body)
    }

    pub(crate) fn loopback() -> Rule {
        Rule { hosts: Vec::new(), loopback_http: true }
    }

    #[test]
    fn refuses_hosts_it_was_not_given() {
        let rule = Rule::hosts(&["github.com"]);
        assert!(rule.allows(&Url::parse("https://github.com/a").unwrap()));
        assert!(rule.allows(&Url::parse("https://GitHub.com/a").unwrap()));
        for url in ["http://github.com/a", "https://evil.example/a", "https://github.com.evil.example/", "http://127.0.0.1:9/"] {
            assert!(!rule.allows(&Url::parse(url).unwrap()), "{url}");
        }
        let client = Client::new(rule, "t");
        let err = client.get(&Url::parse("https://example.org/").unwrap(), &[]).err().unwrap();
        assert!(matches!(err, Error::Refused(_)), "{err}");
    }

    #[test]
    fn length_chunked_and_redirects() {
        let fixture = serve(|head, _| {
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            match path.as_str() {
                "/len" => b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
                "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;x=y\r\nabc\r\n4\r\ndefg\r\n0\r\nX-T: 1\r\n\r\n".to_vec(),
                "/moved" => b"HTTP/1.1 302 Found\r\nLocation: /len\r\nContent-Length: 0\r\n\r\n".to_vec(),
                "/away" => b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.example/x\r\nContent-Length: 0\r\n\r\n".to_vec(),
                _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            }
        });
        let client = Client::new(loopback(), "decibell-test");
        let get = |path: &str| client.get(&Url::parse(&format!("{}{path}", fixture.base)).unwrap(), &[]);
        let (_, r) = get("/len").unwrap();
        assert_eq!(r.read_capped(100).unwrap(), b"hello");
        let (_, r) = get("/chunked").unwrap();
        assert_eq!(r.read_capped(100).unwrap(), b"abcdefg");
        let (url, r) = get("/moved").unwrap();
        assert_eq!(url.path(), "/len");
        assert_eq!(r.read_capped(100).unwrap(), b"hello");
        assert!(matches!(get("/away").err().unwrap(), Error::Refused(_)), "a redirect to a host not given");
        let (_, r) = get("/len").unwrap();
        assert!(r.read_capped(3).is_err(), "the cap");
        let head = &fixture.requests.lock().unwrap()[0];
        assert!(head.starts_with("GET /len HTTP/1.1\r\n") && head.contains("User-Agent: decibell-test") && head.contains("Connection: close"), "{head}");
    }

    #[test]
    fn https_with_a_trusted_root() {
        use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["localhost".to_owned()]).unwrap().signed_by(&leaf_key, &ca).unwrap();
        let der = leaf.der().clone();
        let root = ca.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
        let server = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![der.clone()], key)
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(server).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut buf = [0u8; 4096];
            let mut seen = Vec::new();
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = tls.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        });
        let client = Client::new(Rule::hosts(&["localhost"]), "t").trust(root);
        let (_, r) = client.get(&Url::parse(&format!("https://localhost:{port}/")).unwrap(), &[]).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.read_capped(10).unwrap(), b"ok");
    }
}
