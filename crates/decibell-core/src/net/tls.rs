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
    policy: Policy,
    seen: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
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
        match &self.policy {
            Policy::Exact(expected) if expected != &fp => {
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
        *self.seen.lock().unwrap() = Some(fp.clone());
        pins::record_seen(&self.host, self.port, &fp);
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

/// TLS connector pinned for one host:port. `seen` receives the
/// fingerprint of the certificate that was accepted.
pub fn create_pinned_connector(host: &str, port: u16) -> (TlsConnector, Arc<Mutex<Option<String>>>) {
    let seen = Arc::new(Mutex::new(None));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinVerifier {
        host: host.to_string(),
        port,
        policy: pins::policy_for(host, port),
        seen: seen.clone(),
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("rustls default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (TlsConnector::from(Arc::new(config)), seen)
}
