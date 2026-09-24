//! Manifest and chunk index data model (serialised as CBOR; field order is fixed by the structs,
//! which keeps the encoding deterministic for identical inputs).

use crate::chunk::ChunkParams;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

fn bytebuf_is_empty(b: &ByteBuf) -> bool {
    b.as_ref().is_empty()
}

fn is_zero_u8(x: &u8) -> bool {
    *x == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkRef {
    /// BLAKE3-256 of the chunk plaintext.
    pub h: ByteBuf,
    pub s: u32,
    /// 0 = stored in this archive; 1 = external (available in a referenced archive, see `Manifest::refs`)
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub x: u8,
}

/// An archive whose chunks this archive references instead of storing ("compression by reference").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefArchive {
    /// Merkle root (content identity) of the referenced archive.
    pub root: ByteBuf,
    /// Advisory file name / locator.
    pub hint: String,
    /// Number of chunks of this archive that live in the referenced one.
    pub chunks: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlobRecord {
    /// Offset inside the blobs section.
    pub off: u64,
    pub clen: u32,
    pub ulen: u32,
    pub codec: u8,
    /// Indices (into the chunk table) of the chunks stored in this blob, in order.
    pub n: Vec<u32>,
    /// Codec parameters needed for decoding (e.g. PPMd: [order, mem_size]); empty for zstd/xz/store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<u32>,
    /// Pre-filter applied before compression: low nibble = id (0 none, 1 x86 BCJ, 2 ARM64 BCJ),
    /// high nibble = parameter (ARM64: byte offset of the first aligned instruction word).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub f: u8,
    /// Codec 5 (zstd+delta): indices of the chunks whose concatenation is the raw-content
    /// dictionary (an earlier version of the data, possibly external). Never delta blobs themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub d: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ChunkIndex {
    pub chunks: Vec<ChunkRef>,
    pub blobs: Vec<BlobRecord>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub max_total: u64,
    pub max_entry: u64,
    pub max_ratio: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_total: 4 << 30, max_entry: 1 << 30, max_ratio: 1000 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZipMemberMeta {
    pub name: String,
    /// 0 = stored (payload is plaintext), 1 = deflate inverted with preflate, 2 = raw compressed bytes kept
    pub kind: u8,
    /// Exact bytes of the local file header (fixed part + name + extra).
    pub local_header: ByteBuf,
    /// Exact bytes of the data descriptor that follows the payload (empty if none).
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub descriptor: ByteBuf,
    /// Exact bytes of the central directory record for this member.
    pub central: ByteBuf,
    /// preflate corrections needed to recreate the deflate stream bit-exact (kind 1 only).
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub corrections: ByteBuf,
    /// Length of the payload that was chunked (plaintext for kinds 0/1, raw bytes for kind 2).
    pub size: u64,
    pub chunks: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZipRecipe {
    /// Member indices in the order their local headers appear in the file.
    pub local_order: Vec<u32>,
    /// Exact bytes from the end-of-central-directory record to the end of the file.
    pub eocd: ByteBuf,
    pub members: Vec<ZipMemberMeta>,
}

/// One segment of a file that embeds zlib/deflate streams (PDF FlateDecode objects and similar).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamSegment {
    /// 0 = literal bytes (chunked as-is), 1 = zlib stream inverted with preflate
    pub kind: u8,
    /// zlib 2-byte header for kind 1 (empty otherwise)
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub header: ByteBuf,
    /// zlib 4-byte Adler-32 trailer for kind 1 (empty otherwise)
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub trailer: ByteBuf,
    #[serde(default, skip_serializing_if = "bytebuf_is_empty")]
    pub corrections: ByteBuf,
    /// Length of the chunked payload (literal bytes or inverted plaintext).
    pub size: u64,
    pub chunks: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamRecipe {
    pub segments: Vec<StreamSegment>,
}

/// A stored derivative generated at pack time from another entry (hybrid fidelity): the source
/// stays bit-exact, the view is pre-computed so that agents read it without converting.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Derived {
    /// Path of the source entry.
    pub from: String,
    /// "markdown" | "text"
    pub view: String,
    /// Tool that produced the view (name/version), for provenance.
    pub generator: String,
    /// Planning estimate (chars / 3.5), not a tokenizer measurement.
    pub tokens_est: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    /// BLAKE3-256 of the original file bytes.
    pub h: ByteBuf,
    /// "raw" | "zip" | "streams"
    pub mode: String,
    #[serde(default)]
    pub chunks: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<ZipRecipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<StreamRecipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Present when this entry is a stored canonical view of another entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<Derived>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub tsaur: u16,
    pub profile: String,
    pub fidelity: String,
    pub chunking: ChunkParams,
    pub solid_block: u32,
    pub limits: Limits,
    /// Merkle root over the chunk table (content identity of the archive).
    pub root: ByteBuf,
    pub entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default)]
    pub generator: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<RefArchive>,
}
