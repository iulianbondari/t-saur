//! Encrypted, peer-authenticated connections for the volume exchange: TLS 1.3 (rustls with the
//! ring provider) between two T-saur instances that each hold a self-signed certificate. There is
//! no certificate authority, account or server of ours: identities are **pinned locally** by the
//! SHA-256 fingerprint of the certificate, which the user copies through a channel they trust
//! (the same channel that carries the set id). A connection is accepted only when the remote
//! certificate hashes to a pinned fingerprint; everything else in the certificate (names, dates,
//! issuer) is ignored on purpose.
//!
//! Revocation is local too (`Revocations`): a text file of fingerprints that this side refuses
//! even when they are pinned or allowed, read once at start-up and checked before the pin, so a
//! lost key can be shut out without touching every allow list. Nothing on the wire can add or
//! remove a revocation.
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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Name presented in the handshake; it carries no meaning because identity is the fingerprint.
pub const PEER_NAME: &str = "tsaur-peer";

/// Largest revocation file accepted (a wrong path such as an archive fails fast).
pub const MAX_REVOCATION_FILE: u64 = 1 << 20;

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

/// Certificate fingerprints refused even when pinned or allowed (`docs/design/VOLUME-TRUST.md`
/// §6.3). One fingerprint per line; `#` starts a comment, blank lines are ignored, colons and
/// letter case are tolerated. Read once; there is no propagation and no expiry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revocations(HashSet<[u8; 32]>);

impl Revocations {
    /// Read and parse a revocation file (at most `MAX_REVOCATION_FILE` bytes).
    pub fn load(path: &Path) -> Result<Revocations> {
        let origin = path.display().to_string();
        let len = std::fs::metadata(path).map_err(|e| Error::Missing(format!("revocation file {origin}: {e}")))?.len();
        if len > MAX_REVOCATION_FILE {
            return Err(Error::Limit(format!("revocation file {origin}: {len} bytes, more than the {MAX_REVOCATION_FILE}-byte limit; is this the right file?")));
        }
        let text = std::fs::read_to_string(path).map_err(|e| Error::Missing(format!("revocation file {origin}: {e}")))?;
        Self::parse(&text, &origin)
    }

    /// Parse the text of a revocation file; `origin` names it in errors.
    pub fn parse(text: &str, origin: &str) -> Result<Revocations> {
        if text.len() as u64 > MAX_REVOCATION_FILE {
            return Err(Error::Limit(format!("revocation file {origin}: more than the {MAX_REVOCATION_FILE}-byte limit")));
        }
        let mut set = HashSet::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let fp = parse_fingerprint(line).map_err(|_| Error::Invalid(format!("revocation file {origin}, line {}: fingerprint must be 64 hex characters", i + 1)))?;
            set.insert(fp);
        }
        Ok(Revocations(set))
    }

    pub fn contains(&self, fp: &[u8; 32]) -> bool {
        self.0.contains(fp)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.0.iter()
    }
}

impl FromIterator<[u8; 32]> for Revocations {
    fn from_iter<I: IntoIterator<Item = [u8; 32]>>(iter: I) -> Self {
        Revocations(iter.into_iter().collect())
    }
}

/// Accepts exactly the pinned server certificate, whatever it says about itself, unless its
/// fingerprint is revoked.
#[derive(Debug)]
struct PinnedServer {
    expected: [u8; 32],
    revoked: Revocations,
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
        let fp = fingerprint(end_entity);
        if self.revoked.contains(&fp) {
            Err(rustls::Error::General(format!("peer certificate fingerprint {} is revoked", hex::encode(fp))))
        } else if fp == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!("peer certificate fingerprint {} does not match the pinned identity", hex::encode(fp))))
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

/// Accepts client certificates whose fingerprint is in the allow list (mutual TLS); a revoked
/// fingerprint is refused first, even when listed.
#[derive(Debug)]
struct PinnedClients {
    allowed: Vec<[u8; 32]>,
    revoked: Revocations,
    algs: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for PinnedClients {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _now: UnixTime) -> std::result::Result<ClientCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        if self.revoked.contains(&fp) {
            Err(rustls::Error::General(format!("client certificate fingerprint {} is revoked", hex::encode(fp))))
        } else if self.allowed.contains(&fp) {
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
    server_config_with(identity, allowed, &Revocations::default())
}

/// `server_config` with a revocation list: a revoked client fingerprint fails the handshake even
/// when it is in `allowed`. Anonymous clients present no certificate, so revocations have no
/// effect when `allowed` is empty.
pub fn server_config_with(identity: &Identity, allowed: &[[u8; 32]], revoked: &Revocations) -> Result<Arc<ServerConfig>> {
    let provider = provider();
    let algs = provider.signature_verification_algorithms;
    let builder = ServerConfig::builder_with_provider(provider).with_protocol_versions(&[&rustls::version::TLS13]).map_err(|e| Error::Crypto(format!("tls: {e}")))?;
    let cert_chain = vec![identity.cert.clone()];
    let config = if allowed.is_empty() {
        builder.with_no_client_auth().with_single_cert(cert_chain, identity.key.clone_key())
    } else {
        builder.with_client_cert_verifier(Arc::new(PinnedClients { allowed: allowed.to_vec(), revoked: revoked.clone(), algs })).with_single_cert(cert_chain, identity.key.clone_key())
    }
    .map_err(|e| Error::Crypto(format!("tls server configuration: {e}")))?;
    Ok(Arc::new(config))
}

/// Client side: accept only the server whose certificate fingerprint is `peer`; present
/// `identity` when the server asks for a client certificate.
pub fn client_config(identity: Option<&Identity>, peer: [u8; 32]) -> Result<Arc<ClientConfig>> {
    client_config_with(identity, peer, &Revocations::default())
}

/// `client_config` with a revocation list: a revoked server fingerprint fails the handshake even
/// when it is the pinned `peer`, before any request is sent.
pub fn client_config_with(identity: Option<&Identity>, peer: [u8; 32], revoked: &Revocations) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let algs = provider.signature_verification_algorithms;
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Crypto(format!("tls: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer { expected: peer, revoked: revoked.clone(), algs }));
    let config = match identity {
        Some(id) => builder.with_client_auth_cert(vec![id.cert.clone()], id.key.clone_key()).map_err(|e| Error::Crypto(format!("tls client configuration: {e}")))?,
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

pub fn server_name() -> ServerName<'static> {
    ServerName::try_from(PEER_NAME.to_string()).expect("constant peer name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revocation_file_parses_comments_blank_lines_and_colons() {
        let a = "3f".repeat(32);
        let b: String = (0..32).map(|_| "7B:").collect::<String>().trim_end_matches(':').to_string();
        let text = format!("# tsaur revocations\n\n{a}   # laptop key lost\r\n  {b}\n\n# trailing comment only\n");
        let r = Revocations::parse(&text, "test").unwrap();
        assert_eq!(r.len(), 2);
        assert!(r.contains(&[0x3f; 32]) && r.contains(&[0x7b; 32]));
        // duplicates are harmless, an empty file is empty
        assert_eq!(Revocations::parse(&format!("{a}\n{a}\n"), "t").unwrap().len(), 1);
        assert!(Revocations::parse("", "t").unwrap().is_empty());
        assert!(Revocations::parse("# only comments\n\n", "t").unwrap().is_empty());
        // a bad line names its number
        let err = Revocations::parse("# ok\n\nnot-a-fingerprint\n", "revoked.txt").unwrap_err().to_string();
        assert!(err.contains("revoked.txt, line 3"), "{err}");
        let err = Revocations::parse(&"ab".repeat(31), "t").unwrap_err().to_string();
        assert!(err.contains("line 1"), "{err}");
        // more than the file limit is refused before parsing
        let big = format!("{a}\n").repeat((MAX_REVOCATION_FILE as usize / 65) + 2);
        assert!(matches!(Revocations::parse(&big, "t"), Err(Error::Limit(_))));
    }
}
