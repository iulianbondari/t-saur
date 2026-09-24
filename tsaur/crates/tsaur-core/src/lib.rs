//! # tsaur-core
//!
//! Reference implementation of the **T-saur** archive format (`.tsr`): a content-addressed,
//! agent-first archive.
//!
//! * content-defined chunking (FastCDC) + BLAKE3 identities + deduplication, Merkle root identity
//! * block mode (default), solid blocks or granular blobs; codecs zstd, zstd + trained dictionary,
//!   xz (LZMA2) and PPMd H, chosen per block; x86 and ARM64 branch filters for machine code
//! * container awareness: ZIP/OPC members and PDF FlateDecode streams inverted with preflate,
//!   JPEG (files and DCTDecode streams) recoded losslessly with Lepton; always rebuilt bit-exact
//! * compression by reference: chunks present in a referenced archive are not stored again
//! * per-blob authenticated encryption (XChaCha20-Poly1305) with an envelope key unlocked by
//!   Argon2id passwords or hybrid X25519 + ML-KEM-768 recipients; Ed25519 signatures
//! * hard resource limits (anti decompression bomb), path sanitisation by construction
//! * streaming packer and reader (bounded memory), range reads, canonical views (DOCX, PDF)
//! * fixed-size pieces + Reed-Solomon parity sidecars for transport verification and recovery
//!
//! The format is specified in `docs/spec/`; `tsaur` (the CLI crate) adds the agent-facing
//! commands and the MCP server.

pub mod canonical;
pub mod chunk;
pub mod codec;
pub mod container;
pub mod crypto;
pub mod error;
pub mod format;
pub mod manifest;
pub mod pack;
pub mod paths;
pub mod pieces;
pub mod read;
pub mod tls;
pub mod transfer;
pub mod volumes;

pub use error::{Error, Result};
pub use pack::{pack, PackOptions, PackReport};
pub use read::{inspect, Inspect, Reader, VerifyReport};

/// Library version string (matches the crate version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Identifies the writer line recorded inside archives, views and volume descriptors. It names the
/// format (v1) and the writer generation, not the crate release, so that the same inputs give the
/// same bytes across patch releases (the golden archives in `tests/golden/` pin this).
pub const WRITER: &str = "tsaur-core/1.0";
