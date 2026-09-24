//! On-disk framing: fixed header, section table, trailer, canonical CBOR helpers.
//!
//! ```text
//! +----------------------------------------------------------------+
//! | header (32 B): "TSR\x1A" | version u16 | flags u16 | hlen u32 | 0 |
//! | sections ... (recipients, dictionary, blobs, chunk index,       |
//! |               manifest, signatures)                             |
//! | section table (CBOR array of {t, off, len, h})                  |
//! | trailer (24 B): table_off u64 | table_len u64 | crc32 u32 | "RST\x1A" |
//! +----------------------------------------------------------------+
//! ```
//! Every section is hashed (BLAKE3) in the table; the table itself is protected by the CRC in
//! the trailer and, when present, by the signature section. There is exactly one source of truth
//! for every value: readers refuse archives whose redundant fields disagree.

use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

pub const MAGIC: [u8; 4] = *b"TSR\x1A";
pub const TRAILER_MAGIC: [u8; 4] = *b"RST\x1A";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 32;
pub const TRAILER_LEN: usize = 24;

pub const FLAG_ENCRYPTED: u16 = 0x0001;
pub const FLAG_SIGNED: u16 = 0x0002;

/// Section type identifiers.
pub mod section {
    pub const RECIPIENTS: u8 = 1;
    pub const DICTIONARY: u8 = 2;
    pub const BLOBS: u8 = 3;
    pub const CHUNK_INDEX: u8 = 4;
    pub const MANIFEST: u8 = 5;
    pub const SIGNATURES: u8 = 6;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionEntry {
    pub t: u8,
    pub off: u64,
    pub len: u64,
    pub h: ByteBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub version: u16,
    pub flags: u16,
}

pub fn encode_header(h: &Header) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..4].copy_from_slice(&MAGIC);
    b[4..6].copy_from_slice(&h.version.to_le_bytes());
    b[6..8].copy_from_slice(&h.flags.to_le_bytes());
    b[8..12].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    b
}

pub fn decode_header(b: &[u8]) -> Result<Header> {
    if b.len() < HEADER_LEN || b[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([b[4], b[5]]);
    if version != VERSION {
        return Err(Error::Version(version));
    }
    let flags = u16::from_le_bytes([b[6], b[7]]);
    let hlen = u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize;
    if hlen != HEADER_LEN {
        return Err(Error::Corrupt("header length field".into()));
    }
    if b[12..HEADER_LEN].iter().any(|&x| x != 0) {
        return Err(Error::Corrupt("reserved header bytes must be zero".into()));
    }
    Ok(Header { version, flags })
}

pub fn encode_trailer(table_off: u64, table_len: u64, crc: u32) -> [u8; TRAILER_LEN] {
    let mut b = [0u8; TRAILER_LEN];
    b[0..8].copy_from_slice(&table_off.to_le_bytes());
    b[8..16].copy_from_slice(&table_len.to_le_bytes());
    b[16..20].copy_from_slice(&crc.to_le_bytes());
    b[20..24].copy_from_slice(&TRAILER_MAGIC);
    b
}

/// Returns (table_off, table_len, crc32).
pub fn decode_trailer(b: &[u8]) -> Result<(u64, u64, u32)> {
    if b.len() != TRAILER_LEN || b[20..24] != TRAILER_MAGIC {
        return Err(Error::Corrupt("trailer magic".into()));
    }
    let off = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let len = u64::from_le_bytes(b[8..16].try_into().unwrap());
    let crc = u32::from_le_bytes(b[16..20].try_into().unwrap());
    Ok((off, len, crc))
}

pub fn cbor_encode<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).map_err(|e| Error::Encoding(format!("cbor encode: {e}")))?;
    Ok(out)
}

pub fn cbor_decode<T: DeserializeOwned>(b: &[u8]) -> Result<T> {
    ciborium::from_reader(b).map_err(|e| Error::Encoding(format!("cbor decode: {e}")))
}

pub fn crc32(b: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(b);
    h.finalize()
}

/// The message covered by the archive signature: header bytes and every non-signature section
/// (type, offset, length, hash), domain separated.
pub fn signing_message(header: &[u8], sections: &[SectionEntry]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"tsaur-sig-v1");
    h.update(header);
    for s in sections {
        if s.t == section::SIGNATURES {
            continue;
        }
        h.update(&[s.t]);
        h.update(&s.off.to_le_bytes());
        h.update(&s.len.to_le_bytes());
        h.update(&s.h);
    }
    *h.finalize().as_bytes()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignatureBlock {
    pub alg: String,
    pub key: ByteBuf,
    pub sig: ByteBuf,
}
