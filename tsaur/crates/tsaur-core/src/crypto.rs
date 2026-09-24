//! Envelope encryption and signatures.
//!
//! * A random 256-bit **archive key** (DEK) never encrypts anything directly; sub-keys are derived
//!   with BLAKE3 `derive_key` (blob key, nonce key, ...).
//! * Every section/blob is sealed with **XChaCha20-Poly1305**; the 24-byte nonce is derived from
//!   the nonce key, a label and the blob index, and the AAD binds the blob header.
//! * The DEK is wrapped by a **KEK** derived from a passphrase with **Argon2id** (RFC 9106);
//!   readers enforce a floor on the KDF parameters. Hybrid X25519 + ML-KEM-768 recipients and
//!   FIDO2 stanzas are the next steps (spec §8).
//! * Signatures: **Ed25519** over the signing message of the section table (ML-DSA to follow).

use crate::error::{Error, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

pub const LABEL_BLOB: u8 = 1;
pub const LABEL_INDEX: u8 = 2;
pub const LABEL_MANIFEST: u8 = 3;
pub const LABEL_DICT: u8 = 4;

pub fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    getrandom::getrandom(&mut v).map_err(|e| Error::Crypto(format!("rng: {e}")))?;
    Ok(v)
}

#[derive(Clone)]
pub struct ArchiveKey(pub [u8; 32]);

impl ArchiveKey {
    pub fn random() -> Result<Self> {
        let v = random_bytes(32)?;
        let mut k = [0u8; 32];
        k.copy_from_slice(&v);
        Ok(ArchiveKey(k))
    }

    fn sub(&self, context: &str) -> [u8; 32] {
        blake3::derive_key(context, &self.0)
    }

    pub fn blob_key(&self) -> [u8; 32] {
        self.sub("tsaur v1 blob key")
    }

    pub fn nonce_key(&self) -> [u8; 32] {
        self.sub("tsaur v1 nonce key")
    }
}

fn nonce_for(key: &ArchiveKey, label: u8, index: u64) -> [u8; 24] {
    let mut m = [0u8; 9];
    m[0] = label;
    m[1..].copy_from_slice(&index.to_le_bytes());
    let h = blake3::keyed_hash(&key.nonce_key(), &m);
    let mut n = [0u8; 24];
    n.copy_from_slice(&h.as_bytes()[..24]);
    n
}

pub fn seal(key: &ArchiveKey, label: u8, index: u64, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.blob_key()));
    let nonce = nonce_for(key, label, index);
    cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad }).map_err(|_| Error::Crypto("encryption failed".into()))
}

pub fn open(key: &ArchiveKey, label: u8, index: u64, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.blob_key()));
    let nonce = nonce_for(key, label, index);
    cipher.decrypt(XNonce::from_slice(&nonce), Payload { msg: ciphertext, aad }).map_err(|_| Error::Crypto("authentication failed (wrong password or corrupt data)".into()))
}

#[derive(Clone, Copy, Debug)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    /// Spec default: 256 MiB, t = 3, p = 4.
    fn default() -> Self {
        Self { m_kib: 256 * 1024, t: 3, p: 4 }
    }
}

impl KdfParams {
    /// Reader-enforced floor (RFC 9106 option 2): 64 MiB, t = 3.
    pub const FLOOR: KdfParams = KdfParams { m_kib: 64 * 1024, t: 3, p: 1 };
}

/// One way to unlock the archive key. `t` selects the stanza type:
/// * `argon2id` — passphrase: `salt`, `m_kib`, `t_cost`, `p`
/// * `x25519mlkem768` — hybrid recipient: `eph` (ephemeral X25519 public key), `ct` (ML-KEM-768 ciphertext)
///
/// In both cases `wrapped` = nonce (24 B) ‖ XChaCha20-Poly1305(DEK) under the derived KEK.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stanza {
    pub t: String,
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub salt: ByteBuf,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub m_kib: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub t_cost: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub p: u32,
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub eph: ByteBuf,
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub ct: ByteBuf,
    pub wrapped: ByteBuf,
}

fn bytebuf_is_empty(b: &ByteBuf) -> bool {
    b.as_ref().is_empty()
}

fn is_zero(x: &u32) -> bool {
    *x == 0
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RecipientsBlock {
    pub stanzas: Vec<Stanza>,
}

/// What a reader can present to unlock an archive.
#[derive(Clone, Default)]
pub struct Credentials {
    pub password: Option<String>,
    pub identities: Vec<hybrid::Identity>,
}

fn argon2id(password: &[u8], salt: &[u8], kp: &KdfParams) -> Result<[u8; 32]> {
    let params = Params::new(kp.m_kib, kp.t, kp.p, Some(32)).map_err(|e| Error::Crypto(format!("argon2 params: {e}")))?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    a.hash_password_into(password, salt, &mut out).map_err(|e| Error::Crypto(format!("argon2: {e}")))?;
    Ok(out)
}

fn wrap_dek(dek: &ArchiveKey, kek: &[u8; 32]) -> Result<Vec<u8>> {
    let nonce = random_bytes(24)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(kek));
    let ct = cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: &dek.0, aad: b"tsaur v1 dek" }).map_err(|_| Error::Crypto("wrap failed".into()))?;
    let mut wrapped = nonce;
    wrapped.extend_from_slice(&ct);
    Ok(wrapped)
}

fn unwrap_dek(wrapped: &[u8], kek: &[u8; 32]) -> Result<ArchiveKey> {
    if wrapped.len() < 24 + 32 + 16 {
        return Err(Error::Corrupt("recipient stanza too short".into()));
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(kek));
    let (nonce, ct) = wrapped.split_at(24);
    let dek = cipher.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: b"tsaur v1 dek" }).map_err(|_| Error::Crypto("wrong password/identity (or corrupt recipient stanza)".into()))?;
    let mut k = [0u8; 32];
    k.copy_from_slice(&dek);
    Ok(ArchiveKey(k))
}

pub fn wrap_password(dek: &ArchiveKey, password: &[u8], kp: &KdfParams) -> Result<Stanza> {
    if kp.m_kib < KdfParams::FLOOR.m_kib || kp.t < KdfParams::FLOOR.t || kp.p < 1 {
        return Err(Error::Invalid("KDF parameters below the allowed floor (64 MiB, t=3)".into()));
    }
    let salt = random_bytes(16)?;
    let kek = argon2id(password, &salt, kp)?;
    let wrapped = wrap_dek(dek, &kek)?;
    Ok(Stanza { t: "argon2id".into(), salt: ByteBuf::from(salt), m_kib: kp.m_kib, t_cost: kp.t, p: kp.p, eph: ByteBuf::new(), ct: ByteBuf::new(), wrapped: ByteBuf::from(wrapped) })
}

pub fn unwrap_password(stanza: &Stanza, password: &[u8]) -> Result<ArchiveKey> {
    if stanza.m_kib < KdfParams::FLOOR.m_kib || stanza.t_cost < KdfParams::FLOOR.t || stanza.p < 1 {
        return Err(Error::Policy("archive KDF parameters are below the allowed floor".into()));
    }
    let kp = KdfParams { m_kib: stanza.m_kib, t: stanza.t_cost, p: stanza.p };
    let kek = argon2id(password, &stanza.salt, &kp)?;
    unwrap_dek(&stanza.wrapped, &kek)
}

/// Try every stanza against the credentials; the first that opens wins.
pub fn unlock(block: &RecipientsBlock, creds: &Credentials) -> Result<ArchiveKey> {
    let mut last = Error::Crypto("archive is encrypted: no matching password or identity".into());
    for s in &block.stanzas {
        match s.t.as_str() {
            "argon2id" => {
                if let Some(pw) = &creds.password {
                    match unwrap_password(s, pw.as_bytes()) {
                        Ok(k) => return Ok(k),
                        Err(e) => last = e,
                    }
                }
            }
            "x25519mlkem768" => {
                for id in &creds.identities {
                    match hybrid::unwrap(s, id) {
                        Ok(k) => return Ok(k),
                        Err(e) => last = e,
                    }
                }
            }
            _ => {}
        }
    }
    Err(last)
}

/// Hybrid X25519 + ML-KEM-768 recipients (FIPS 203; combiner as in RFC 10024 / OpenSSH mlkem768x25519):
/// KEK = HKDF-SHA-256(ss_mlkem ‖ ss_x25519, salt = eph_pub ‖ BLAKE3(ct), info = "tsaur v1 x25519mlkem768").
/// Confidentiality holds if either component holds, which protects archives against
/// "harvest now, decrypt later".
pub mod hybrid {
    use super::*;
    use hkdf::Hkdf;
    use kem::{Decapsulate, Encapsulate, FromSeed, KeyExport};
    use ml_kem::MlKem768;
    use sha2::Sha256;
    use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

    pub const IDENTITY_LEN: usize = 32 + 64;
    pub const RECIPIENT_LEN: usize = 32 + 1184;
    pub const CT_LEN: usize = 1088;

    /// Secret identity: X25519 static secret (32 B) + ML-KEM-768 seed (64 B).
    #[derive(Clone)]
    pub struct Identity {
        x_secret: [u8; 32],
        k_seed: [u8; 64],
    }

    /// Public recipient: X25519 public key (32 B) + ML-KEM-768 encapsulation key (1184 B).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Recipient {
        pub x_pub: [u8; 32],
        pub k_ek: Vec<u8>,
    }

    impl Identity {
        pub fn generate() -> Result<Self> {
            let x = random_bytes(32)?;
            let s = random_bytes(64)?;
            let mut x_secret = [0u8; 32];
            x_secret.copy_from_slice(&x);
            let mut k_seed = [0u8; 64];
            k_seed.copy_from_slice(&s);
            Ok(Self { x_secret, k_seed })
        }

        pub fn to_bytes(&self) -> Vec<u8> {
            let mut v = self.x_secret.to_vec();
            v.extend_from_slice(&self.k_seed);
            v
        }

        pub fn from_bytes(b: &[u8]) -> Result<Self> {
            if b.len() != IDENTITY_LEN {
                return Err(Error::Invalid(format!("identity must be {IDENTITY_LEN} bytes")));
            }
            let mut x_secret = [0u8; 32];
            x_secret.copy_from_slice(&b[..32]);
            let mut k_seed = [0u8; 64];
            k_seed.copy_from_slice(&b[32..]);
            Ok(Self { x_secret, k_seed })
        }

        pub fn recipient(&self) -> Recipient {
            let x_pub = PublicKey::from(&StaticSecret::from(self.x_secret)).to_bytes();
            let seed = kem::Seed::<MlKem768>::from(self.k_seed);
            let (_dk, ek) = MlKem768::from_seed(&seed);
            Recipient { x_pub, k_ek: ek.to_bytes().to_vec() }
        }
    }

    impl Recipient {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut v = self.x_pub.to_vec();
            v.extend_from_slice(&self.k_ek);
            v
        }

        pub fn from_bytes(b: &[u8]) -> Result<Self> {
            if b.len() != RECIPIENT_LEN {
                return Err(Error::Invalid(format!("recipient key must be {RECIPIENT_LEN} bytes")));
            }
            let mut x_pub = [0u8; 32];
            x_pub.copy_from_slice(&b[..32]);
            Ok(Self { x_pub, k_ek: b[32..].to_vec() })
        }
    }

    fn kek(ss_kem: &[u8], ss_x: &[u8], eph_pub: &[u8; 32], ct: &[u8]) -> [u8; 32] {
        let mut ikm = ss_kem.to_vec();
        ikm.extend_from_slice(ss_x);
        let mut salt = eph_pub.to_vec();
        salt.extend_from_slice(blake3::hash(ct).as_bytes());
        let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
        let mut out = [0u8; 32];
        hk.expand(b"tsaur v1 x25519mlkem768", &mut out).expect("hkdf output length");
        out
    }

    pub fn wrap(dek: &ArchiveKey, r: &Recipient) -> Result<Stanza> {
        let ek_arr = kem::Key::<ml_kem::EncapsulationKey<MlKem768>>::try_from(&r.k_ek[..]).map_err(|_| Error::Invalid("bad ML-KEM encapsulation key length".into()))?;
        let ek = ml_kem::EncapsulationKey::<MlKem768>::new(&ek_arr).map_err(|_| Error::Invalid("invalid ML-KEM encapsulation key".into()))?;
        let (ct, ss_kem) = ek.encapsulate();
        let eph = EphemeralSecret::random();
        let eph_pub = PublicKey::from(&eph).to_bytes();
        let ss_x = eph.diffie_hellman(&PublicKey::from(r.x_pub));
        let k = kek(ss_kem.as_slice(), ss_x.as_bytes(), &eph_pub, ct.as_slice());
        Ok(Stanza {
            t: "x25519mlkem768".into(),
            salt: ByteBuf::new(),
            m_kib: 0,
            t_cost: 0,
            p: 0,
            eph: ByteBuf::from(eph_pub.to_vec()),
            ct: ByteBuf::from(ct.as_slice().to_vec()),
            wrapped: ByteBuf::from(wrap_dek(dek, &k)?),
        })
    }

    pub fn unwrap(stanza: &Stanza, id: &Identity) -> Result<ArchiveKey> {
        if stanza.eph.len() != 32 || stanza.ct.len() != CT_LEN {
            return Err(Error::Corrupt("malformed hybrid stanza".into()));
        }
        let seed = kem::Seed::<MlKem768>::from(id.k_seed);
        let (dk, _ek) = MlKem768::from_seed(&seed);
        let ct = kem::Ciphertext::<MlKem768>::try_from(&stanza.ct[..]).map_err(|_| Error::Corrupt("bad ML-KEM ciphertext length".into()))?;
        let ss_kem = dk.decapsulate(&ct);
        let mut eph_pub = [0u8; 32];
        eph_pub.copy_from_slice(&stanza.eph);
        let ss_x = StaticSecret::from(id.x_secret).diffie_hellman(&PublicKey::from(eph_pub));
        let k = kek(ss_kem.as_slice(), ss_x.as_bytes(), &eph_pub, &stanza.ct);
        unwrap_dek(&stanza.wrapped, &k)
    }
}

/// Ed25519 signatures (composite ML-DSA-65 planned).
pub mod sig {
    use super::*;
    use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

    /// Returns (secret seed, public key).
    pub fn keygen() -> Result<([u8; 32], [u8; 32])> {
        let v = random_bytes(32)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&v);
        let sk = SigningKey::from_bytes(&seed);
        Ok((seed, sk.verifying_key().to_bytes()))
    }

    pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(seed).verifying_key().to_bytes()
    }

    pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(seed).sign(msg).to_bytes()
    }

    pub fn verify(pubkey: &[u8; 32], msg: &[u8], sig: &[u8]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(pubkey) else { return false };
        let Ok(sig_arr): std::result::Result<[u8; 64], _> = sig.try_into() else { return false };
        let s = Signature::from_bytes(&sig_arr);
        vk.verify_strict(msg, &s).is_ok()
    }
}
