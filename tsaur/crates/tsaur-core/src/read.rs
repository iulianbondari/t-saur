//! Reading: open → verify framing and every section hash → (unwrap key) → decode index and
//! manifest → serve chunks, ranges, entries; verify; extract with safe paths.

use crate::chunk;
use crate::codec;
use crate::container;
use crate::crypto::{self, ArchiveKey, Credentials, RecipientsBlock};
use crate::error::{Error, Result};
use crate::format::{self, section, Header, SectionEntry, SignatureBlock};
use crate::manifest::{ChunkIndex, Entry, Manifest};
use crate::pack::{blob_aad, zstd_unwrap};
use crate::paths;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Hard ceilings applied by every reader regardless of what the manifest declares.
pub const READER_MAX_TOTAL: u64 = 64 << 30;
pub const READER_MAX_SECTION: usize = 1 << 30;
/// Entries a manifest may declare (`docs/V1-CONTRACT.md` §4).
pub const READER_MAX_ENTRIES: usize = 10_000_000;
/// Plaintext cache budget for decoded blobs.
pub const CACHE_BYTES: usize = 64 << 20;

enum Backing {
    Mem(Vec<u8>),
    Map(memmap2::Mmap),
}

impl std::ops::Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Backing::Mem(v) => v,
            Backing::Map(m) => m,
        }
    }
}

pub struct Reader {
    data: Backing,
    pub header: Header,
    pub sections: Vec<SectionEntry>,
    pub manifest: Manifest,
    pub index: ChunkIndex,
    dict: Option<Vec<u8>>,
    key: Option<ArchiveKey>,
    blobs_off: u64,
    blob_of_chunk: Vec<u32>,
    cache: HashMap<u32, Arc<Vec<u8>>>,
    signature: Option<SignatureBlock>,
    /// Reference archives that resolve external chunks (see `Manifest::refs`).
    refs: Vec<Reader>,
    ref_lookup: HashMap<[u8; 32], (usize, u32)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyReport {
    pub sections_ok: bool,
    pub entries_total: usize,
    pub entries_ok: usize,
    pub entries_bad: Vec<String>,
    pub blobs: usize,
    pub signature: Option<String>,
    pub signature_valid: Option<bool>,
}

/// Longest chain of delta dictionaries a reader will follow.
pub const MAX_DELTA_DEPTH: u8 = 16;

/// Depth of the delta chain below blob `bi` (0 for a plain blob); rejects cycles, self-references
/// and chains deeper than `MAX_DELTA_DEPTH`. Recursion is bounded by that depth.
fn delta_depth(bi: usize, blobs: &[crate::manifest::BlobRecord], blob_of_chunk: &[u32], memo: &mut [u8], visiting: &mut [bool]) -> Result<u8> {
    if blobs[bi].codec != codec::ZSTD_DELTA {
        return Ok(0);
    }
    if memo[bi] != 0 {
        return Ok(memo[bi]);
    }
    if visiting[bi] {
        return Err(Error::Corrupt(format!("delta blob {bi}: dictionary cycle")));
    }
    visiting[bi] = true;
    let mut depth = 1u8;
    for &ci in &blobs[bi].d {
        let owner = blob_of_chunk[ci as usize];
        if owner == u32::MAX {
            continue; // external chunk: resolved by a reference archive
        }
        if owner as usize == bi {
            return Err(Error::Corrupt(format!("delta blob {bi}: dictionary made of its own chunks")));
        }
        let below = delta_depth(owner as usize, blobs, blob_of_chunk, memo, visiting)?;
        depth = depth.max(below.saturating_add(1));
        if depth > MAX_DELTA_DEPTH {
            return Err(Error::Limit(format!("delta blob {bi}: dictionary chain deeper than {MAX_DELTA_DEPTH}")));
        }
    }
    visiting[bi] = false;
    memo[bi] = depth;
    Ok(depth)
}

/// Parse and validate the container framing: header, trailer, section table (CRC) and every
/// section's BLAKE3 hash. Needs no credentials.
fn framing(data: &[u8]) -> Result<(Header, Vec<SectionEntry>)> {
    if data.len() < format::HEADER_LEN + format::TRAILER_LEN {
        return Err(Error::Corrupt("file too short".into()));
    }
    let header = format::decode_header(&data[..format::HEADER_LEN])?;
    let (toff, tlen, crc) = format::decode_trailer(&data[data.len() - format::TRAILER_LEN..])?;
    let tend = toff.checked_add(tlen).ok_or_else(|| Error::Corrupt("table range".into()))?;
    if toff < format::HEADER_LEN as u64 || tend != (data.len() - format::TRAILER_LEN) as u64 {
        return Err(Error::Corrupt("section table does not end at the trailer".into()));
    }
    let table = &data[toff as usize..tend as usize];
    if format::crc32(table) != crc {
        return Err(Error::Corrupt("section table crc".into()));
    }
    let sections: Vec<SectionEntry> = format::cbor_decode(table)?;

    // Every section: inside the file, non-overlapping, hash verified.
    let mut cursor = format::HEADER_LEN as u64;
    for s in &sections {
        let end = s.off.checked_add(s.len).ok_or_else(|| Error::Corrupt("section range overflow".into()))?;
        if s.off < cursor || end > toff {
            return Err(Error::Corrupt(format!("section {} out of bounds or overlapping", s.t)));
        }
        if s.len as usize > READER_MAX_SECTION && s.t != section::BLOBS {
            return Err(Error::Limit(format!("section {} too large", s.t)));
        }
        let bytes = &data[s.off as usize..(s.off + s.len) as usize];
        if chunk::hash(bytes)[..] != s.h[..] {
            return Err(Error::HashMismatch(format!("section {}", s.t)));
        }
        cursor = s.off + s.len;
    }
    Ok((header, sections))
}

/// What can be learned about an archive without credentials: framing, sections, how it can be
/// unlocked (stanza types and KDF parameters) and whether it is signed.
#[derive(Clone, Debug, Serialize)]
pub struct Inspect {
    pub version: u16,
    pub encrypted: bool,
    pub signed: bool,
    pub file_bytes: u64,
    pub sections: Vec<SectionEntry>,
    pub stanzas: Vec<StanzaInfo>,
    pub signature: Option<SignatureBlock>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StanzaInfo {
    pub t: String,
    pub m_kib: u32,
    pub t_cost: u32,
    pub p: u32,
}

pub fn inspect(path: &Path) -> Result<Inspect> {
    let file = std::fs::File::open(path)?;
    let data = Backing::Map(unsafe { memmap2::Mmap::map(&file)? });
    let (header, sections) = framing(&data)?;
    let find = |t: u8| sections.iter().find(|s| s.t == t).map(|s| &data[s.off as usize..(s.off + s.len) as usize]);
    let stanzas = match find(section::RECIPIENTS) {
        Some(b) => {
            let block: RecipientsBlock = format::cbor_decode(b)?;
            block.stanzas.iter().map(|s| StanzaInfo { t: s.t.clone(), m_kib: s.m_kib, t_cost: s.t_cost, p: s.p }).collect()
        }
        None => Vec::new(),
    };
    let signature = match find(section::SIGNATURES) {
        Some(b) => Some(format::cbor_decode::<SignatureBlock>(b)?),
        None => None,
    };
    Ok(Inspect {
        version: header.version,
        encrypted: header.flags & format::FLAG_ENCRYPTED != 0,
        signed: header.flags & format::FLAG_SIGNED != 0,
        file_bytes: data.len() as u64,
        sections,
        stanzas,
        signature,
    })
}

impl Reader {
    /// Open an archive file (memory-mapped; nothing is decoded before the framing and every
    /// section hash have been verified).
    pub fn open(path: &Path, password: Option<&str>) -> Result<Reader> {
        Self::open_with_refs(path, password, &[])
    }

    /// Open an archive together with the reference archives that hold its external chunks.
    pub fn open_with_refs(path: &Path, password: Option<&str>, references: &[PathBuf]) -> Result<Reader> {
        let creds = Credentials { password: password.map(|s| s.to_string()), identities: Vec::new() };
        Self::open_with(path, &creds, references)
    }

    /// Open with full credentials (passphrase and/or hybrid identities) and reference archives.
    pub fn open_with(path: &Path, creds: &Credentials, references: &[PathBuf]) -> Result<Reader> {
        let mut r = Self::open_one(path, creds)?;
        for rp in references {
            let rr = Self::open_one(rp, creds).map_err(|e| Error::Missing(format!("reference archive {}: {e}", rp.display())))?;
            let ri = r.refs.len();
            for (ci, c) in rr.index.chunks.iter().enumerate() {
                if c.x == 0 && c.h.len() == 32 {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&c.h);
                    r.ref_lookup.entry(h).or_insert((ri, ci as u32));
                }
            }
            r.refs.push(rr);
        }
        Ok(r)
    }

    /// Number of external chunks that cannot be resolved with the references given so far.
    pub fn unresolved_external(&self) -> usize {
        self.index
            .chunks
            .iter()
            .filter(|c| c.x != 0)
            .filter(|c| {
                let mut h = [0u8; 32];
                if c.h.len() != 32 {
                    return true;
                }
                h.copy_from_slice(&c.h);
                !self.ref_lookup.contains_key(&h)
            })
            .count()
    }

    fn open_one(path: &Path, creds: &Credentials) -> Result<Reader> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the mapping is read-only; concurrent modification of the file by another process
        // would be detected by the section hashes (or cause a hash-mismatch error), never memory unsafety
        // in this crate's own logic because every access is bounds-checked against the mapped length.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_backing(Backing::Map(map), creds)
    }

    pub fn from_bytes(data: Vec<u8>, password: Option<&str>) -> Result<Reader> {
        let creds = Credentials { password: password.map(|s| s.to_string()), identities: Vec::new() };
        Self::from_backing(Backing::Mem(data), &creds)
    }

    fn from_backing(data: Backing, creds: &Credentials) -> Result<Reader> {
        if data.len() < format::HEADER_LEN + format::TRAILER_LEN {
            return Err(Error::Corrupt("file too short".into()));
        }
        let (header, sections) = framing(&data)?;
        let find = |t: u8| sections.iter().find(|s| s.t == t).map(|s| &data[s.off as usize..(s.off + s.len) as usize]);

        let key = if header.flags & format::FLAG_ENCRYPTED != 0 {
            if creds.password.is_none() && creds.identities.is_empty() {
                return Err(Error::Crypto("archive is encrypted: password or identity required".into()));
            }
            let block: RecipientsBlock = format::cbor_decode(find(section::RECIPIENTS).ok_or_else(|| Error::Corrupt("missing recipients section".into()))?)?;
            Some(crypto::unlock(&block, creds)?)
        } else {
            None
        };

        let open_section = |label: u8, aad: &[u8], bytes: &[u8]| -> Result<Vec<u8>> {
            match &key {
                Some(k) => crypto::open(k, label, 0, aad, bytes),
                None => Ok(bytes.to_vec()),
            }
        };

        let index_raw = open_section(crypto::LABEL_INDEX, b"index", find(section::CHUNK_INDEX).ok_or_else(|| Error::Corrupt("missing chunk index".into()))?)?;
        let index: ChunkIndex = format::cbor_decode(&zstd_unwrap(&index_raw, READER_MAX_SECTION)?)?;
        let manifest_raw = open_section(crypto::LABEL_MANIFEST, b"manifest", find(section::MANIFEST).ok_or_else(|| Error::Corrupt("missing manifest".into()))?)?;
        let manifest: Manifest = format::cbor_decode(&zstd_unwrap(&manifest_raw, READER_MAX_SECTION)?)?;
        if manifest.entries.len() > READER_MAX_ENTRIES {
            return Err(Error::Limit(format!("{} entries exceed the reader limit of {READER_MAX_ENTRIES}", manifest.entries.len())));
        }
        if manifest.tsaur != 1 {
            return Err(Error::Version(manifest.tsaur));
        }
        let dict = match find(section::DICTIONARY) {
            Some(b) => Some(open_section(crypto::LABEL_DICT, b"dict", b)?),
            None => None,
        };
        let blobs_off = sections.iter().find(|s| s.t == section::BLOBS).map(|s| s.off).ok_or_else(|| Error::Corrupt("missing blobs section".into()))?;
        let signature = match find(section::SIGNATURES) {
            Some(b) => Some(format::cbor_decode::<SignatureBlock>(b)?),
            None => None,
        };

        // Limits: declared sizes vs. reader ceilings, bomb ratio, index consistency.
        let total: u64 = index.blobs.iter().map(|b| b.ulen as u64).sum();
        let max_total = manifest.limits.max_total.min(READER_MAX_TOTAL);
        if total > max_total {
            return Err(Error::Limit(format!("declared {total} bytes exceed limit {max_total}")));
        }
        let ratio = total / (data.len() as u64).max(1);
        if ratio > manifest.limits.max_ratio as u64 {
            return Err(Error::Limit(format!("expansion ratio {ratio}x exceeds limit")));
        }
        let mut blob_of_chunk = vec![u32::MAX; index.chunks.len()];
        let blobs_len = sections.iter().find(|s| s.t == section::BLOBS).map(|s| s.len).unwrap_or(0);
        for (bi, b) in index.blobs.iter().enumerate() {
            if b.off.checked_add(b.clen as u64).is_none_or(|e| e > blobs_len) {
                return Err(Error::Corrupt(format!("blob {bi} out of bounds")));
            }
            if b.n.is_empty() || b.n.iter().any(|&ci| ci as usize >= index.chunks.len()) {
                return Err(Error::Corrupt(format!("blob {bi} references chunks out of range")));
            }
            let sum: u64 = b.n.iter().map(|&ci| index.chunks[ci as usize].s as u64).sum();
            if sum != b.ulen as u64 {
                return Err(Error::Corrupt(format!("blob {bi} size does not match its chunks")));
            }
            for &ci in &b.n {
                if blob_of_chunk[ci as usize] != u32::MAX {
                    return Err(Error::Corrupt(format!("chunk {ci} referenced by two blobs")));
                }
                blob_of_chunk[ci as usize] = bi as u32;
            }
        }
        for (ci, c) in index.chunks.iter().enumerate() {
            if c.h.len() != 32 {
                return Err(Error::Corrupt(format!("chunk {ci}: bad hash length")));
            }
            if c.x == 0 && blob_of_chunk[ci] == u32::MAX {
                return Err(Error::Corrupt(format!("chunk {ci} without blob")));
            }
            if c.x != 0 && blob_of_chunk[ci] != u32::MAX {
                return Err(Error::Corrupt(format!("chunk {ci} marked external but stored")));
            }
        }
        // Delta blobs: dictionary chunks in range and bounded; the dependency graph between blobs
        // (a delta blob depends on the blobs storing its dictionary chunks) must be acyclic and at
        // most MAX_DELTA_DEPTH deep, so decoding always terminates after a bounded amount of work.
        for (bi, b) in index.blobs.iter().enumerate() {
            if b.codec != codec::ZSTD_DELTA {
                continue;
            }
            if b.d.is_empty() || b.d.iter().any(|&ci| ci as usize >= index.chunks.len()) {
                return Err(Error::Corrupt(format!("delta blob {bi}: dictionary chunks out of range")));
            }
            let dict_bytes: u64 = b.d.iter().map(|&ci| index.chunks[ci as usize].s as u64).sum();
            if dict_bytes > 64 << 20 {
                return Err(Error::Limit(format!("delta blob {bi}: dictionary larger than 64 MiB")));
            }
        }
        let mut memo = vec![0u8; index.blobs.len()];
        let mut visiting = vec![false; index.blobs.len()];
        for bi in 0..index.blobs.len() {
            if index.blobs[bi].codec == codec::ZSTD_DELTA {
                delta_depth(bi, &index.blobs, &blob_of_chunk, &mut memo, &mut visiting)?;
            }
        }
        for e in &manifest.entries {
            paths::normalize(&e.path)?;
            let refs: Box<dyn Iterator<Item = &u32>> = match (&e.zip, &e.streams) {
                (Some(z), _) => Box::new(z.members.iter().flat_map(|m| m.chunks.iter())),
                (None, Some(s)) => Box::new(s.segments.iter().flat_map(|m| m.chunks.iter())),
                (None, None) => Box::new(e.chunks.iter()),
            };
            for &ci in refs {
                if ci as usize >= index.chunks.len() {
                    return Err(Error::Corrupt(format!("{}: chunk reference out of range", e.path)));
                }
            }
        }

        Ok(Reader { data, header, sections, manifest, index, dict, key, blobs_off, blob_of_chunk, cache: HashMap::new(), signature, refs: Vec::new(), ref_lookup: HashMap::new() })
    }

    pub fn is_encrypted(&self) -> bool {
        self.key.is_some()
    }

    pub fn signature(&self) -> Option<&SignatureBlock> {
        self.signature.as_ref()
    }

    /// Verify the embedded signature against a public key (hex or raw 32 bytes).
    pub fn verify_signature(&self, pubkey: &[u8; 32]) -> Result<bool> {
        let s = self.signature.as_ref().ok_or_else(|| Error::Missing("archive is not signed".into()))?;
        if s.alg != "ed25519" {
            return Err(Error::Crypto(format!("unsupported signature algorithm {}", s.alg)));
        }
        if s.key[..] != pubkey[..] {
            return Ok(false);
        }
        let msg = format::signing_message(&self.data[..format::HEADER_LEN], &self.sections);
        Ok(crypto::sig::verify(pubkey, &msg, &s.sig))
    }

    fn blob_plain(&mut self, bi: u32) -> Result<Arc<Vec<u8>>> {
        if let Some(p) = self.cache.get(&bi) {
            return Ok(p.clone());
        }
        let b = self.index.blobs[bi as usize].clone();
        let start = (self.blobs_off + b.off) as usize;
        let stored = &self.data[start..start + b.clen as usize];
        let aad = blob_aad(b.codec, b.ulen, b.n[0], b.n.len() as u32);
        let compressed = match &self.key {
            Some(k) => crypto::open(k, crypto::LABEL_BLOB, bi as u64, &aad, stored)?,
            None => stored.to_vec(),
        };
        let plain = if b.codec == codec::ZSTD_DELTA {
            // the dictionary is an earlier version of this content: chunks of this archive or of a reference
            let mut prefix = Vec::with_capacity(b.d.iter().map(|&ci| self.index.chunks[ci as usize].s as usize).sum());
            for &ci in &b.d {
                prefix.extend_from_slice(&self.chunk(ci)?);
            }
            codec::zstd_decompress_with_prefix(&compressed, b.ulen as usize, &prefix)?
        } else {
            codec::decompress_with(b.codec, &compressed, b.ulen as usize, self.dict.as_deref(), &b.p, b.f)?
        };
        // Verify every chunk hash inside the blob.
        let mut off = 0usize;
        for &ci in &b.n {
            let c = &self.index.chunks[ci as usize];
            let piece = &plain[off..off + c.s as usize];
            if chunk::hash(piece)[..] != c.h[..] {
                return Err(Error::HashMismatch(format!("chunk {ci}")));
            }
            off += c.s as usize;
        }
        let arc = Arc::new(plain);
        // bounded plaintext cache: keeps memory predictable for large archives
        let cached: usize = self.cache.values().map(|v| v.len()).sum();
        if cached + arc.len() > CACHE_BYTES {
            self.cache.clear();
        }
        self.cache.insert(bi, arc.clone());
        Ok(arc)
    }

    /// Stream the bytes of an entry to `sink` (raw entries chunk by chunk; container entries rebuilt)
    /// and verify the entry hash; returns the number of bytes written.
    pub fn stream_entry(&mut self, ei: usize, sink: &mut dyn std::io::Write) -> Result<u64> {
        let e = self.manifest.entries.get(ei).cloned().ok_or_else(|| Error::Missing(format!("entry {ei}")))?;
        if e.mode != "raw" {
            let bytes = self.entry_bytes(ei)?;
            sink.write_all(&bytes)?;
            return Ok(bytes.len() as u64);
        }
        let mut hasher = blake3::Hasher::new();
        let mut total = 0u64;
        for &ci in &e.chunks {
            let c = self.chunk(ci)?;
            hasher.update(&c);
            total += c.len() as u64;
            sink.write_all(&c)?;
        }
        if total != e.size || hasher.finalize().as_bytes()[..] != e.h[..] {
            return Err(Error::HashMismatch(e.path.clone()));
        }
        Ok(total)
    }

    /// Plaintext of one chunk (verified); external chunks are fetched from the reference archives.
    pub fn chunk(&mut self, ci: u32) -> Result<Vec<u8>> {
        let c = &self.index.chunks[ci as usize];
        if c.x != 0 {
            let mut h = [0u8; 32];
            h.copy_from_slice(&c.h);
            let (ri, cj) = *self.ref_lookup.get(&h).ok_or_else(|| Error::Missing(format!("chunk {ci} is external (b3:{}…) and no reference archive provides it", hex::encode(&h[..6]))))?;
            let data = self.refs[ri].chunk(cj)?;
            if chunk::hash(&data) != h {
                return Err(Error::HashMismatch(format!("external chunk {ci}")));
            }
            return Ok(data);
        }
        let bi = self.blob_of_chunk[ci as usize];
        let b = &self.index.blobs[bi as usize];
        let mut off = 0usize;
        for &k in &b.n {
            if k == ci {
                break;
            }
            off += self.index.chunks[k as usize].s as usize;
        }
        let len = self.index.chunks[ci as usize].s as usize;
        let plain = self.blob_plain(bi)?;
        Ok(plain[off..off + len].to_vec())
    }

    fn stream(&mut self, refs: &[u32]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for &ci in refs {
            out.extend_from_slice(&self.chunk(ci)?);
        }
        Ok(out)
    }

    /// Full bytes of an entry, verified against its BLAKE3 hash.
    pub fn entry_bytes(&mut self, ei: usize) -> Result<Vec<u8>> {
        let e = self.manifest.entries.get(ei).cloned().ok_or_else(|| Error::Missing(format!("entry {ei}")))?;
        let data = match e.mode.as_str() {
            "raw" => self.stream(&e.chunks)?,
            "zip" => {
                let z = e.zip.as_ref().ok_or_else(|| Error::Corrupt("zip entry without recipe".into()))?;
                let mut parts = Vec::with_capacity(z.members.len());
                for m in &z.members {
                    parts.push(self.stream(&m.chunks)?);
                }
                container::rebuild(z, &parts)?
            }
            "streams" => {
                let s = e.streams.as_ref().ok_or_else(|| Error::Corrupt("streams entry without recipe".into()))?;
                let mut parts = Vec::with_capacity(s.segments.len());
                for seg in &s.segments {
                    parts.push(self.stream(&seg.chunks)?);
                }
                container::rebuild_streams(s, &parts)?
            }
            other => return Err(Error::Corrupt(format!("unknown entry mode {other}"))),
        };
        if data.len() as u64 != e.size || chunk::hash(&data)[..] != e.h[..] {
            return Err(Error::HashMismatch(e.path.clone()));
        }
        Ok(data)
    }

    /// Byte range of a raw entry without materialising the whole file (zip entries are rebuilt).
    pub fn read_range(&mut self, ei: usize, start: u64, len: usize) -> Result<Vec<u8>> {
        let e = self.manifest.entries.get(ei).cloned().ok_or_else(|| Error::Missing(format!("entry {ei}")))?;
        if e.mode != "raw" {
            let all = self.entry_bytes(ei)?;
            let s = (start as usize).min(all.len());
            let t = (s + len).min(all.len());
            return Ok(all[s..t].to_vec());
        }
        let end = start.saturating_add(len as u64).min(e.size);
        let mut out = Vec::new();
        let mut pos = 0u64;
        for &ci in &e.chunks {
            let sz = self.index.chunks[ci as usize].s as u64;
            let (cs, ce) = (pos, pos + sz);
            if ce > start && cs < end {
                let c = self.chunk(ci)?;
                let from = start.saturating_sub(cs) as usize;
                let to = (end - cs).min(sz) as usize;
                out.extend_from_slice(&c[from..to]);
            }
            pos = ce;
            if pos >= end {
                break;
            }
        }
        Ok(out)
    }

    pub fn find_entry(&self, path: &str) -> Option<usize> {
        let norm = paths::normalize(path).ok()?;
        self.manifest.entries.iter().position(|e| e.path == norm)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.manifest.entries
    }

    /// Extract everything (or the selected entry indices) under `dir`, with sanitised paths.
    /// Raw entries stream chunk by chunk to a temporary file that is renamed into place only after
    /// the hash verified; container entries are rebuilt in memory. Existing files are replaced
    /// only with `overwrite`; every destination is checked before the first byte is written, so
    /// a refusal leaves the directory as it was.
    pub fn extract(&mut self, dir: &Path, selected: Option<&[usize]>, overwrite: bool) -> Result<Vec<PathBuf>> {
        let idxs: Vec<usize> = match selected {
            Some(s) => s.to_vec(),
            // stored views (.tsaur/views/…) are extracted only when selected explicitly
            None => (0..self.manifest.entries.len()).filter(|&i| self.manifest.entries[i].derived.is_none()).collect(),
        };
        for &i in &idxs {
            let e = self.manifest.entries.get(i).ok_or_else(|| Error::Missing(format!("entry {i}")))?;
            let dest = paths::safe_join(dir, &e.path)?;
            match std::fs::symlink_metadata(&dest) {
                Ok(m) if m.is_dir() => return Err(Error::Policy(format!("{} exists and is a directory", dest.display()))),
                Ok(_) if !overwrite => return Err(Error::Policy(format!("{} exists; pass --overwrite to replace existing files", dest.display()))),
                _ => {}
            }
        }
        // On a case-insensitive filesystem two entries that differ only by letter case would
        // overwrite each other silently; refuse before writing anything.
        if cfg!(any(windows, target_os = "macos")) {
            let mut seen = std::collections::HashSet::new();
            for &i in &idxs {
                let p = &self.manifest.entries.get(i).ok_or_else(|| Error::Missing(format!("entry {i}")))?.path;
                if !seen.insert(p.to_lowercase()) {
                    return Err(Error::Policy(format!("entries differ only by letter case ({p}): refusing to extract on a case-insensitive filesystem; select entries explicitly")));
                }
            }
        }
        let mut written = Vec::new();
        for ei in idxs {
            let e = self.manifest.entries.get(ei).cloned().ok_or_else(|| Error::Missing(format!("entry {ei}")))?;
            let dest = paths::safe_join(dir, &e.path)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = dest.with_extension(format!("{}.tsr-partial", dest.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_default()));
            {
                let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
                let res = self.stream_entry(ei, &mut f).and_then(|_| std::io::Write::flush(&mut f).map_err(Error::from));
                drop(f);
                if let Err(err) = res {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(err);
                }
            }
            if dest.exists() {
                std::fs::remove_file(&dest)?;
            }
            std::fs::rename(&tmp, &dest)?;
            written.push(dest);
        }
        Ok(written)
    }

    /// Decompress and hash-check every entry; verify the signature if a public key is given.
    pub fn verify(&mut self, pubkey: Option<&[u8; 32]>) -> Result<VerifyReport> {
        let mut bad = Vec::new();
        let n = self.manifest.entries.len();
        for ei in 0..n {
            if let Err(e) = self.stream_entry(ei, &mut std::io::sink()) {
                bad.push(format!("{}: {}", self.manifest.entries[ei].path, e));
            }
        }
        let signature = self.signature.as_ref().map(|s| hex::encode(&s.key));
        let signature_valid = match (pubkey, &self.signature) {
            (Some(pk), Some(_)) => Some(self.verify_signature(pk)?),
            _ => None,
        };
        Ok(VerifyReport { sections_ok: true, entries_total: n, entries_ok: n - bad.len(), entries_bad: bad, blobs: self.index.blobs.len(), signature, signature_valid })
    }
}
