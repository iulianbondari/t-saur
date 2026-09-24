//! Encrypted, peer-authenticated connections for the volume exchange: TLS 1.3 (rustls with the
//! ring provider) between two T-saur instances that each hold a self-signed certificate. There is
//! no certificate authority, account or server of ours: identities are **pinned locally** by the
//! SHA-256 fingerprint of the certificate, which the user copies through a channel they trust
//! (the same channel that carries the set id). A connection is accepted only when the remote
//! certificate hashes to a pinned fingerprint; everything else in the certificate (names, dates,
//! issuer) is ignored on purpose.
//!
//! Files: `<identity>` holds the PKCS#8 private key (PEM), `<identity>.crt` the certificate (PEM).
//! `fingerprint()` is what a user shares; the private key never leaves the machine.

use crate::error::{Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Name presented in the handshake; it carries no meaning because identity is the fingerprint.
pub const PEER_NAME: &str = "tsaur-peer";

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint: [u8; 32],
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Identity { cert: self.cert.clone(), key: self.key.clone_key(), fingerprint: self.fingerprint }
    }
}

pub fn fingerprint(cert: &CertificateDer<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(cert.as_ref());
    h.finalize().into()
}

pub fn cert_path(identity: &Path) -> PathBuf {
    PathBuf::from(format!("{}.crt", identity.display()))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Generate a fresh ECDSA P-256 key and a self-signed certificate; write `<path>` (key) and
/// `<path>.crt` (certificate) and return the identity.
pub fn generate(path: &Path) -> Result<Identity> {
    let key = rcgen::KeyPair::generate().map_err(|e| Error::Crypto(format!("key generation: {e}")))?;
    let params = rcgen::CertificateParams::new(vec![PEER_NAME.to_string()]).map_err(|e| Error::Crypto(format!("certificate parameters: {e}")))?;
    let cert = params.self_signed(&key).map_err(|e| Error::Crypto(format!("self-signed certificate: {e}")))?;
    std::fs::write(path, key.serialize_pem())?;
    std::fs::write(cert_path(path), cert.pem())?;
    load(path)
}

/// Load an identity written by `generate`.
pub fn load(path: &Path) -> Result<Identity> {
    let key_pem = std::fs::read(path).map_err(|e| Error::Missing(format!("identity {}: {e}", path.display())))?;
    let cert_pem = std::fs::read(cert_path(path)).map_err(|e| Error::Missing(format!("certificate {}: {e}", cert_path(path).display())))?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|e| Error::Crypto(format!("identity {}: {e}", path.display())))?;
    let cert = CertificateDer::from_pem_slice(&cert_pem).map_err(|e| Error::Crypto(format!("certificate {}: {e}", cert_path(path).display())))?;
    let fingerprint = fingerprint(&cert);
    Ok(Identity { cert, key, fingerprint })
}

pub fn parse_fingerprint(hex_str: &str) -> Result<[u8; 32]> {
    let v = hex::decode(hex_str.trim().replace(':', "")).map_err(|_| Error::Invalid("fingerprint must be 64 hex characters".into()))?;
    let arr: [u8; 32] = v.as_slice().try_into().map_err(|_| Error::Invalid("fingerprint must be 32 bytes".into()))?;
    Ok(arr)
}

/// Accepts exactly the pinned server certificate, whatever it says about itself.
#[derive(Debug)]
struct PinnedServer {
    expected: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!("peer certificate fingerprint {} does not match the pinned identity", hex::encode(fingerprint(end_entity)))))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Accepts client certificates whose fingerprint is in the allow list (mutual TLS).
#[derive(Debug)]
struct PinnedClients {
    allowed: Vec<[u8; 32]>,
    algs: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for PinnedClients {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _now: UnixTime) -> std::result::Result<ClientCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        if self.allowed.contains(&fp) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!("client certificate fingerprint {} is not in the allow list", hex::encode(fp))))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Server side: present `identity`; when `allowed` is non-empty require a client certificate
/// whose fingerprint is listed (mutual authentication), otherwise accept anonymous clients
/// (the connection is still encrypted and the server is still authenticated by the client).
pub fn server_config(identity: &Identity, allowed: &[[u8; 32]]) -> Result<Arc<ServerConfig>> {
    let provider = provider();
    let algs = provider.signature_verification_algorithms;
    let builder = ServerConfig::builder_with_provider(provider).with_protocol_versions(&[&rustls::version::TLS13]).map_err(|e| Error::Crypto(format!("tls: {e}")))?;
    let cert_chain = vec![identity.cert.clone()];
    let config = if allowed.is_empty() {
        builder.with_no_client_auth().with_single_cert(cert_chain, identity.key.clone_key())
    } else {
        builder.with_client_cert_verifier(Arc::new(PinnedClients { allowed: allowed.to_vec(), algs })).with_single_cert(cert_chain, identity.key.clone_key())
    }
    .map_err(|e| Error::Crypto(format!("tls server configuration: {e}")))?;
    Ok(Arc::new(config))
}

/// Client side: accept only the server whose certificate fingerprint is `peer`; present
/// `identity` when the server asks for a client certificate.
pub fn client_config(identity: Option<&Identity>, peer: [u8; 32]) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let algs = provider.signature_verification_algorithms;
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Crypto(format!("tls: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer { expected: peer, algs }));
    let config = match identity {
        Some(id) => builder.with_client_auth_cert(vec![id.cert.clone()], id.key.clone_key()).map_err(|e| Error::Crypto(format!("tls client configuration: {e}")))?,
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

pub fn server_name() -> ServerName<'static> {
    ServerName::try_from(PEER_NAME.to_string()).expect("constant peer name")
}
