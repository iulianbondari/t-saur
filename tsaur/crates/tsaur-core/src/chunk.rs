//! Content-defined chunking (FastCDC 2020, normalized chunking level 2) and BLAKE3 identities.

use crate::error::{Error, Result};
use fastcdc::v2020::{FastCDC, Normalization};
use serde::{Deserialize, Serialize};

pub type Hash = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkParams {
    pub min: u32,
    pub avg: u32,
    pub max: u32,
}

impl ChunkParams {
    /// Small text corpora: fine-grained dedup, more metadata.
    pub const FINE: ChunkParams = ChunkParams { min: 2 * 1024, avg: 8 * 1024, max: 64 * 1024 };
    /// Default: aligned with Hugging Face Xet / casync (~64 KiB), P2P friendly.
    pub const P2P: ChunkParams = ChunkParams { min: 16 * 1024, avg: 64 * 1024, max: 256 * 1024 };
    /// Large archives: fewer chunks, less metadata.
    pub const ARCHIVE: ChunkParams = ChunkParams { min: 64 * 1024, avg: 256 * 1024, max: 1024 * 1024 };

    pub fn validate(&self) -> Result<()> {
        if self.min < 64 || self.avg < 256 || self.max < 1024 || self.min > self.avg || self.avg > self.max {
            return Err(Error::Invalid(format!("chunk parameters out of range: {self:?}")));
        }
        Ok(())
    }

    pub fn by_name(name: &str) -> Option<ChunkParams> {
        match name {
            "fine" => Some(Self::FINE),
            "p2p" => Some(Self::P2P),
            "archive" => Some(Self::ARCHIVE),
            _ => None,
        }
    }
}

impl Default for ChunkParams {
    fn default() -> Self {
        Self::P2P
    }
}

/// Chunk boundaries as (offset, length) pairs covering `data` exactly.
pub fn boundaries(data: &[u8], p: &ChunkParams) -> Vec<(usize, usize)> {
    if data.is_empty() {
        return Vec::new();
    }
    FastCDC::with_level(data, p.min, p.avg, p.max, Normalization::Level2).map(|c| (c.offset, c.length)).collect()
}

pub fn hash(data: &[u8]) -> Hash {
    *blake3::hash(data).as_bytes()
}

/// Binary Merkle root with domain separation (RFC 9162 style: 0x00 for leaves, 0x01 for nodes).
pub fn merkle_root(hashes: &[Hash]) -> Hash {
    if hashes.is_empty() {
        return hash(b"");
    }
    let mut level: Vec<Hash> = hashes
        .iter()
        .map(|h| {
            let mut m = Vec::with_capacity(33);
            m.push(0u8);
            m.extend_from_slice(h);
            hash(&m)
        })
        .collect();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            let last = *level.last().unwrap();
            level.push(last);
        }
        level = level
            .chunks(2)
            .map(|pair| {
                let mut m = Vec::with_capacity(65);
                m.push(1u8);
                m.extend_from_slice(&pair[0]);
                m.extend_from_slice(&pair[1]);
                hash(&m)
            })
            .collect();
    }
    level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_cover_input() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let b = boundaries(&data, &ChunkParams::FINE);
        let total: usize = b.iter().map(|x| x.1).sum();
        assert_eq!(total, data.len());
        assert!(b.len() > 10);
        assert_eq!(b[0].0, 0);
    }

    #[test]
    fn merkle_is_stable() {
        let a = merkle_root(&[hash(b"a"), hash(b"b"), hash(b"c")]);
        let b = merkle_root(&[hash(b"a"), hash(b"b"), hash(b"c")]);
        assert_eq!(a, b);
        assert_ne!(a, merkle_root(&[hash(b"a"), hash(b"b")]));
    }
}
