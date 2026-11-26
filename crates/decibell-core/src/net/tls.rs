use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio_rustls::TlsConnector;

use super::pins::{self, Policy};

/// Fingerprint-pinning verifier (Theme A). Self-signed certificates never
/// chain to a CA, so WebPKI validation is meaningless here; identity is
/// the sha256 of the leaf certificate, checked against what central told
/// us (communities) or what we saw first (central / unknown hosts). The
/// handshake signature is still verified, so a pinned certificate can't
/// be replayed by someone without its private key.
#[derive(Debug)]
struct PinVerifier {
    host: String,
    port: u16,
    pin: Pin,
    provider: Arc<CryptoProvider>,
}

/// Where a verifier's expectation comes from.
#[derive(Debug)]
enum Pin {
    /// The pins store (`pins.rs`): its policy for host:port, and the accepted
    /// certificate recorded back there and reported through `seen`.
    Store { policy: Policy, seen: Arc<Mutex<Option<String>>> },
    /// Exactly this fingerprint; the store is neither read nor written.
    Exact(String),
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Policy::Exact(fp) => write!(f, "Exact({})", fp),
            Policy::Tofu => write!(f, "Tofu"),
        }
    }
}

pub fn fingerprint_hex(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fp = fingerprint_hex(end_entity.as_ref());
        let expected = match &self.pin {
            Pin::Store { policy: Policy::Exact(expected), .. } | Pin::Exact(expected) => Some(expected),
            Pin::Store { policy: Policy::Tofu, .. } => None,
        };
        match expected {
            Some(expected) if expected != &fp => {
                log::error!(
                    "TLS pin mismatch for {}:{}: expected {}, got {}",
                    self.host, self.port, expected, fp
                );
                return Err(Error::General(format!(
                    "CERT_MISMATCH:{}:{}:{}",
                    self.host, self.port, fp
                )));
            }
            _ => {}
        }
        if let Pin::Store { seen, .. } = &self.pin {
            *seen.lock().unwrap() = Some(fp.clone());
            pins::record_seen(&self.host, self.port, &fp);
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A connector that trusts what `pin` says about host:port: the ring
/// provider, TLS 1.3 and 1.2 (rustls' safe defaults).
fn pinned(host: &str, port: u16, pin: Pin) -> Result<TlsConnector, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinVerifier {
        host: host.to_string(),
        port,
        pin,
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// TLS connector pinned for one host:port. `seen` receives the
/// fingerprint of the certificate that was accepted.
pub fn create_pinned_connector(host: &str, port: u16) -> (TlsConnector, Arc<Mutex<Option<String>>>) {
    let seen = Arc::new(Mutex::new(None));
    let pin = Pin::Store { policy: pins::policy_for(host, port), seen: seen.clone() };
    let connector = pinned(host, port, pin).expect("rustls default protocol versions");
    (connector, seen)
}

/// A connector for host:port that accepts only the certificate whose
/// sha256 is `fingerprint` (hex, any case, as central reports it): e.g. a
/// community's attachment HTTPS port, pinned to the fingerprint central
/// gives for the community. Side-effect free: unlike
/// [`create_pinned_connector`] it never reads or writes the pins store
/// (`cert_pins.json`), so it works before or without `state::set_boot`. Same
/// verification and the same `CERT_MISMATCH:<host>:<port>:<seen>` error as
/// the pinned connector; TLS 1.3 and 1.2 (the attachment server is 1.2
/// only). `host` / `port` only label that error. Fails on a fingerprint
/// that isn't 64 hex digits.
pub fn exact_connector(host: &str, port: u16, fingerprint: &str) -> Result<TlsConnector, String> {
    let fingerprint = fingerprint.to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("Not a sha256 certificate fingerprint: '{}'", fingerprint));
    }
    pinned(host, port, Pin::Exact(fingerprint)).map_err(|e| format!("TLS config: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::TlsAcceptor;

    /// A loopback TLS server with a fresh self-signed certificate; returns
    /// its port and the certificate's fingerprint. Each accepted connection
    /// answers "pong" to whatever it reads.
    async fn server(versions: &[&'static rustls::SupportedProtocolVersion]) -> (u16, String) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = cert.cert.der().clone();
        let fp = fingerprint_hex(der.as_ref());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
        let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        let mut buf = [0u8; 4];
                        if tls.read_exact(&mut buf).await.is_ok() {
                            let _ = tls.write_all(b"pong").await;
                            let _ = tls.shutdown().await;
                        }
                    }
                });
            }
        });
        (port, fp)
    }

    async fn connect(
        port: u16,
        connector: TlsConnector,
    ) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
        let name = ServerName::try_from("127.0.0.1").unwrap().to_owned();
        connector.connect(name, tcp).await
    }

    #[tokio::test]
    async fn exact_connector_accepts_only_its_fingerprint_and_touches_no_pins() {
        for (label, versions) in [
            ("TLS 1.2 only, like the attachment server", &[&rustls::version::TLS12][..]),
            ("TLS 1.3", &[&rustls::version::TLS13][..]),
        ] {
            let (port, fp) = server(versions).await;

            // The right fingerprint (any case) connects and talks.
            let mut tls = connect(port, exact_connector("127.0.0.1", port, &fp.to_uppercase()).unwrap())
                .await
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let negotiated = tls.get_ref().1.protocol_version();
            assert_eq!(negotiated, Some(versions[0].version), "{label}");
            tls.write_all(b"ping").await.unwrap();
            let mut reply = [0u8; 4];
            tls.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"pong");

            // Any other fails the handshake with the pin contract's error,
            // naming what the server presented.
            let other = "ab".repeat(32);
            let err = connect(port, exact_connector("127.0.0.1", port, &other).unwrap())
                .await
                .err()
                .unwrap_or_else(|| panic!("{label}: a wrong fingerprint connected"));
            let msg = err.to_string();
            assert!(
                msg.contains(&format!("CERT_MISMATCH:127.0.0.1:{port}:{fp}")),
                "{label}: {msg}"
            );

            // Neither attempt read or wrote the pins store.
            assert_eq!(pins::peek(&pins::key("127.0.0.1", port)), None);
        }
        assert!(exact_connector("h", 1, "").is_err());
        assert!(exact_connector("h", 1, &"zz".repeat(32)).is_err());
        assert!(exact_connector("h", 1, &"ab".repeat(31)).is_err());
    }

    /// The local test stack's TLS endpoints (tools/dev/local-stack.sh), for
    /// the provider switch: connect with each fingerprint and print what was
    /// negotiated. `DECIBELL_TLS_PROBE=host:port=fingerprint,…`.
    #[tokio::test]
    #[ignore = "needs the local test stack"]
    async fn probe_local_stack_tls() {
        let spec = std::env::var("DECIBELL_TLS_PROBE").expect("DECIBELL_TLS_PROBE");
        for target in spec.split(',') {
            let (addr, fp) = target.split_once('=').unwrap();
            let (host, port) = addr.rsplit_once(':').unwrap();
            let port: u16 = port.parse().unwrap();
            let tcp = TcpStream::connect((host, port)).await.unwrap();
            let name = ServerName::try_from(host.to_string()).unwrap();
            let tls = exact_connector(host, port, fp).unwrap().connect(name, tcp).await.unwrap();
            let conn = tls.get_ref().1;
            println!(
                "{addr}: {:?} {:?} kx={:?}",
                conn.protocol_version().unwrap(),
                conn.negotiated_cipher_suite().unwrap().suite(),
                conn.negotiated_key_exchange_group().map(|g| g.name())
            );
        }
    }
}
