"""tsaurproto - Python prototype (proof-of-concept) for the T-saur format.

NOT the final implementation (that one will be in Rust). Purpose: validating the ideas with real numbers:
  - content-defined chunking (FastCDC-like, gear hash) + hash-based deduplication
  - zstd dictionary trained on the corpus ("compression by references")
  - container-aware: DOCX (zip) exploded into members, PDF with Flate streams expanded
  - canonical representation for agents (markdown) - "semantic-lossless" mode
  - CBOR manifest + Merkle root (BLAKE2b-256 as a stand-in for BLAKE3)
  - per-chunk encryption XChaCha20-Poly1305 + Argon2id
  - "pieces" for P2P + Reed-Solomon parity (recovery like PAR2/RAR)
"""
__version__ = "0.0.1"
