//! Packing: enumerate → (invert containers) → chunk → dedup → group into blocks → compress → seal
//! → stream to disk. Memory stays bounded by the block size and the chunk index: file contents are
//! chunked with a streaming FastCDC reader, unique chunks are compressed in blocks and written as
//! soon as a batch is ready, and only container files (which need preflate) are held in memory.
//!
//! The output is deterministic for identical inputs and options when `keep_mtime` and `timestamp`
//! are off and no encryption is used (encryption adds a random archive key).

use crate::canonical;
use crate::chunk::{self, ChunkParams, Hash};
use crate::codec::{self, CodecChoice, CodecOptions};
use crate::container;
use crate::crypto::{self, ArchiveKey, KdfParams};
use crate::error::{Error, Result};
use crate::format::{self, section, Header, SectionEntry, SignatureBlock};
use crate::manifest::{BlobRecord, ChunkIndex, ChunkRef, Derived, Entry, Limits, Manifest, RefArchive};
use crate::paths;
use crate::read::Reader;
use fastcdc::v2020::{Normalization, StreamCDC};
use rayon::prelude::*;
use serde::Serialize;
use serde_bytes::ByteBuf;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct PackOptions {
    pub chunk: ChunkParams,
    /// 0 = granular (one chunk per blob, best random access); otherwise consecutive unique chunks
    /// are compressed together in blocks of up to this many bytes (default 1 MiB).
    pub block_size: u32,
    pub level: i32,
    /// Codec(s) the packer may use per block.
    pub codec: CodecChoice,
    /// Train a zstd dictionary on the first `dict_warmup` bytes of unique chunks (granular mode only).
    pub train_dict: bool,
    pub dict_size: usize,
    pub dict_warmup: usize,
    pub container_aware: bool,
    pub password: Option<String>,
    pub kdf: KdfParams,
    pub sign_seed: Option<[u8; 32]>,
    pub keep_mtime: bool,
    pub timestamp: bool,
    pub profile: String,
    pub limits: Limits,
    /// Blocks compressed in parallel per batch (0 = number of CPU threads).
    pub batch: usize,
    /// Reference archives: chunks already present in them are referenced, not stored.
    pub references: Vec<PathBuf>,
    /// Hybrid X25519 + ML-KEM-768 recipients (public key bytes); each gets its own stanza.
    pub recipients: Vec<crypto::hybrid::Recipient>,
    /// Also store canonical views (DOCX -> Markdown, PDF -> text) as derived entries under
    /// `.tsaur/views/` (hybrid fidelity: originals stay bit-exact, views are pre-computed).
    pub canonical: bool,
    /// Delta coding: blocks of an entry that looks like a new version of an earlier entry (same
    /// path in a reference archive, or a similarly named earlier entry of this archive) are also
    /// tried with zstd against that earlier content as dictionary; the smaller encoding wins.
    pub delta: bool,
    /// Recompress JPEG files and PDF `DCTDecode` images with Lepton when the build has it.
    /// `false` (`pack --no-lepton`) stores them as they are, so that every build, including one
    /// without the `lepton` feature, can read the archive.
    pub jpeg_recompression: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            chunk: ChunkParams::P2P,
            block_size: 1 << 20,
            level: 19,
            codec: CodecChoice::Best,
            train_dict: true,
            dict_size: 110 * 1024,
            dict_warmup: 8 << 20,
            container_aware: true,
            password: None,
            kdf: KdfParams::default(),
            sign_seed: None,
            keep_mtime: false,
            timestamp: false,
            profile: "block".into(),
            limits: Limits::default(),
            batch: 0,
            references: Vec::new(),
            recipients: Vec::new(),
            canonical: false,
            delta: true,
            jpeg_recompression: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PackReport {
    pub archive_bytes: u64,
    pub input_bytes: u64,
    pub logical_bytes: u64,
    pub unique_bytes: u64,
    pub chunks_total: usize,
    pub chunks_unique: usize,
    pub blobs: usize,
    pub dict_bytes: usize,
    pub codec_hist: BTreeMap<String, u32>,
    pub entries: usize,
    pub containers_exploded: usize,
    pub containers_fallback: usize,
    pub root: String,
    pub encrypted: bool,
    pub signed: bool,
    pub referenced_chunks: usize,
    pub referenced_bytes: u64,
    /// Stored canonical views (derived entries).
    pub views: usize,
    /// Symbolic links found inside input directories and skipped (never followed).
    pub skipped_links: usize,
}

/// A block of unique chunks waiting for compression.
struct Block {
    chunks: Vec<u32>,
    plain: Vec<u8>,
    /// Dictionary chunk indices for delta coding (an earlier version of this content), if any.
    delta: Option<Vec<u32>>,
}

/// Bytes of recently stored chunks kept for intra-archive delta dictionaries.
const DELTA_WINDOW: usize = 32 << 20;
/// Upper bound of one delta dictionary.
const DELTA_DICT_MAX: usize = 4 << 20;
/// Files up to this size are read whole to confirm a delta candidate; larger ones stream without delta.
const DELTA_WHOLE_FILE_MAX: u64 = 64 << 20;
/// Chunks stored this deep in a chain of delta dictionaries are not used as dictionary material
/// (readers accept chains up to 16; keeping them short keeps random access cheap).
const MAX_WRITER_DELTA_DEPTH: u8 = 6;

enum DictSource {
    /// Chunks of an entry in reference archive `ri`: (hash, size), stored there, external here.
    Ref { ri: usize, chunks: Vec<(Hash, u32)> },
    /// Chunks of an earlier entry of this archive whose bytes are still in the recent window.
    Local { chunks: Vec<u32> },
}

struct DeltaCandidate {
    source: DictSource,
}

/// Do the new bytes share at least a quarter of their content with `old`, judged on 8 KiB
/// content-defined pieces (fine enough that a document with edits every few KB still qualifies,
/// strict enough that two unrelated documents built from the same template do not)?
fn shares_content(old: &[u8], parts: &[&[u8]]) -> bool {
    if old.is_empty() {
        return false;
    }
    let fine = ChunkParams::FINE;
    let prints: HashSet<Hash> = chunk::boundaries(old, &fine).into_iter().map(|(off, len)| chunk::hash(&old[off..off + len])).collect();
    let (mut shared, mut total) = (0usize, 0usize);
    for part in parts {
        for (off, len) in chunk::boundaries(part, &fine) {
            total += len;
            if prints.contains(&chunk::hash(&part[off..off + len])) {
                shared += len;
            }
        }
    }
    shared > 0 && shared * 4 >= total
}

/// Chunk indices of an entry in storage order (raw, ZIP members or stream segments).
fn entry_chunk_indices(e: &Entry) -> Vec<u32> {
    match (&e.zip, &e.streams) {
        (Some(z), _) => z.members.iter().flat_map(|m| m.chunks.iter().copied()).collect(),
        (None, Some(s)) => s.segments.iter().flat_map(|x| x.chunks.iter().copied()).collect(),
        _ => e.chunks.clone(),
    }
}

/// Bucket key for candidate search: extension and the first three bytes of the file name
/// (candidates need a common name prefix of at least three bytes, so they share the bucket).
fn name_key(path: &str) -> Option<(String, [u8; 3])> {
    let name = file_name(path).as_bytes();
    if name.len() < 3 {
        return None;
    }
    Some((extension(path).to_string(), [name[0], name[1], name[2]]))
}

type NameIndex = HashMap<(String, [u8; 3]), Vec<usize>>;

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn extension(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// Length of the common prefix of two file names, in bytes (on char boundaries).
fn common_prefix(a: &str, b: &str) -> usize {
    let n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let mut n = n;
    while !a.is_char_boundary(n) {
        n -= 1;
    }
    n
}

struct Packer {
    opts: PackOptions,
    out: BufWriter<File>,
    pos: u64,
    header: [u8; format::HEADER_LEN],
    sections: Vec<SectionEntry>,
    key: Option<ArchiveKey>,
    // chunk table
    hashes: Vec<Hash>,
    sizes: Vec<u32>,
    external: Vec<u8>,
    index: HashMap<Hash, u32>,
    // reference archives ("compression by reference"): hash -> (reference index, chunk index there)
    ref_lookup: HashMap<Hash, (usize, u32)>,
    ref_meta: Vec<RefArchive>,
    refs: Vec<Reader>,
    /// Per reference archive: exact path -> entry index, and name buckets for similar names.
    ref_path_index: Vec<HashMap<String, usize>>,
    ref_name_index: Vec<NameIndex>,
    /// Name buckets of this archive's entries (delta candidate search).
    name_index: NameIndex,
    // delta coding: dictionary of the entry being added, and a window of recent chunk bytes
    cur_delta: Option<Vec<u32>>,
    /// Per chunk: depth of the delta chain of the block that stores it (0 = plain block).
    delta_depth: Vec<u8>,
    recent: VecDeque<(u32, Arc<Vec<u8>>)>,
    recent_map: HashMap<u32, Arc<Vec<u8>>>,
    recent_bytes: usize,
    referenced_chunks: usize,
    referenced_bytes: u64,
    // blocks and blobs
    current: Option<Block>,
    ready: Vec<Block>,
    blob_records: Vec<BlobRecord>,
    blobs_off: u64,
    blobs_written: u64,
    codec_hist: BTreeMap<String, u32>,
    // dictionary warm-up (granular mode)
    dict: Option<Vec<u8>>,
    dict_done: bool,
    warmup: Vec<(u32, Vec<u8>)>,
    warmup_len: usize,
    // stats
    entries: Vec<Entry>,
    input_bytes: u64,
    logical_bytes: u64,
    unique_bytes: u64,
    chunk_refs: usize,
    exploded: usize,
    fallback: usize,
    views: usize,
}

impl Packer {
    fn new(opts: PackOptions, out_path: &Path) -> Result<Self> {
        opts.chunk.validate()?;
        let key = if opts.password.is_some() || !opts.recipients.is_empty() { Some(ArchiveKey::random()?) } else { None };
        let mut flags = 0u16;
        if key.is_some() {
            flags |= format::FLAG_ENCRYPTED;
        }
        if opts.sign_seed.is_some() {
            flags |= format::FLAG_SIGNED;
        }
        let header = format::encode_header(&Header { version: format::VERSION, flags });
        // read+write: the blobs section is re-read from disk to hash it without holding it in memory
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out_path)?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.write_all(&header)?;
        // Reference archives: collect every chunk hash they hold.
        let mut ref_lookup: HashMap<Hash, (usize, u32)> = HashMap::new();
        let mut ref_meta = Vec::new();
        let mut refs = Vec::new();
        let mut ref_path_index = Vec::new();
        let mut ref_name_index = Vec::new();
        for (i, rp) in opts.references.iter().enumerate() {
            let r = Reader::open(rp, opts.password.as_deref()).map_err(|e| Error::Missing(format!("reference archive {}: {e}", rp.display())))?;
            for (cj, c) in r.index.chunks.iter().enumerate() {
                if c.x == 0 && c.h.len() == 32 {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&c.h);
                    ref_lookup.entry(h).or_insert((i, cj as u32));
                }
            }
            ref_meta.push(RefArchive { root: r.manifest.root.clone(), hint: rp.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), chunks: 0, bytes: 0 });
            let mut paths = HashMap::new();
            let mut names: NameIndex = HashMap::new();
            for (ei, e) in r.manifest.entries.iter().enumerate() {
                if e.derived.is_some() {
                    continue;
                }
                paths.entry(e.path.clone()).or_insert(ei);
                if let Some(k) = name_key(&e.path) {
                    names.entry(k).or_default().push(ei);
                }
            }
            ref_path_index.push(paths);
            ref_name_index.push(names);
            refs.push(r);
        }
        let mut p = Self {
            opts,
            out,
            pos: header.len() as u64,
            header,
            sections: Vec::new(),
            key,
            hashes: Vec::new(),
            sizes: Vec::new(),
            external: Vec::new(),
            index: HashMap::new(),
            ref_lookup,
            ref_meta,
            refs,
            ref_path_index,
            ref_name_index,
            name_index: HashMap::new(),
            cur_delta: None,
            delta_depth: Vec::new(),
            recent: VecDeque::new(),
            recent_map: HashMap::new(),
            recent_bytes: 0,
            referenced_chunks: 0,
            referenced_bytes: 0,
            current: None,
            ready: Vec::new(),
            blob_records: Vec::new(),
            blobs_off: 0,
            blobs_written: 0,
            codec_hist: BTreeMap::new(),
            dict: None,
            dict_done: false,
            warmup: Vec::new(),
            warmup_len: 0,
            entries: Vec::new(),
            input_bytes: 0,
            logical_bytes: 0,
            unique_bytes: 0,
            chunk_refs: 0,
            exploded: 0,
            fallback: 0,
            views: 0,
        };
        if let Some(k) = &p.key {
            let mut block = crypto::RecipientsBlock::default();
            if let Some(pw) = &p.opts.password {
                block.stanzas.push(crypto::wrap_password(k, pw.as_bytes(), &p.opts.kdf)?);
            }
            for r in &p.opts.recipients {
                block.stanzas.push(crypto::hybrid::wrap(k, r)?);
            }
            let bytes = format::cbor_encode(&block)?;
            p.write_section(section::RECIPIENTS, &bytes)?;
        }
        // dictionary is trained later; in block mode there is none
        if p.opts.block_size > 0 || !p.opts.train_dict {
            p.dict_done = true;
        }
        p.blobs_off = p.pos;
        Ok(p)
    }

    fn write_section(&mut self, t: u8, bytes: &[u8]) -> Result<()> {
        let off = self.pos;
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        self.sections.push(SectionEntry { t, off, len: bytes.len() as u64, h: ByteBuf::from(chunk::hash(bytes).to_vec()) });
        Ok(())
    }

    fn batch_size(&self) -> usize {
        if self.opts.batch > 0 {
            self.opts.batch
        } else {
            rayon::current_num_threads().max(1)
        }
    }

    /// Blocks are compressed in batches; a batch is bounded both by count (threads) and by bytes
    /// so that memory stays predictable whatever the block size.
    const MAX_INFLIGHT_BYTES: usize = 128 << 20;

    fn batch_ready(&self) -> bool {
        self.ready.len() >= self.batch_size() || self.ready.iter().map(|b| b.plain.len()).sum::<usize>() >= Self::MAX_INFLIGHT_BYTES
    }

    /// Register a chunk (dedup) and return its index; new chunks are queued for compression.
    fn ingest_chunk(&mut self, data: &[u8]) -> Result<u32> {
        let h = chunk::hash(data);
        self.chunk_refs += 1;
        self.logical_bytes += data.len() as u64;
        if let Some(&i) = self.index.get(&h) {
            return Ok(i);
        }
        let idx = self.hashes.len() as u32;
        self.hashes.push(h);
        self.sizes.push(data.len() as u32);
        self.delta_depth.push(0);
        self.index.insert(h, idx);
        // Present in a reference archive: record it as external and store nothing.
        if let Some(&(ri, _)) = self.ref_lookup.get(&h) {
            self.external.push(1);
            self.referenced_chunks += 1;
            self.referenced_bytes += data.len() as u64;
            self.ref_meta[ri].chunks += 1;
            self.ref_meta[ri].bytes += data.len() as u64;
            return Ok(idx);
        }
        self.external.push(0);
        self.unique_bytes += data.len() as u64;
        self.remember(idx, data);

        if self.opts.block_size == 0 {
            if !self.dict_done {
                self.warmup.push((idx, data.to_vec()));
                self.warmup_len += data.len();
                if self.warmup_len >= self.opts.dict_warmup {
                    self.finish_warmup()?;
                }
            } else {
                self.ready.push(Block { chunks: vec![idx], plain: data.to_vec(), delta: None });
                if self.batch_ready() {
                    self.flush_ready()?;
                }
            }
        } else {
            // a block never mixes chunks with different delta dictionaries
            let full = match &self.current {
                // a versioned entry starts a new block when the current one carries a different
                // dictionary or holds the dictionary chunks themselves (a blob can never be its own dictionary)
                Some(b) => {
                    (b.plain.len() + data.len() > self.opts.block_size as usize && !b.plain.is_empty())
                        || self.cur_delta.as_ref().is_some_and(|d| (b.delta.is_some() && b.delta.as_ref() != Some(d)) || d.iter().any(|ci| b.chunks.contains(ci)))
                }
                None => false,
            };
            if full {
                let b = self.current.take().unwrap();
                self.ready.push(b);
                if self.batch_ready() {
                    self.flush_ready()?;
                }
            }
            match &mut self.current {
                Some(b) => {
                    b.chunks.push(idx);
                    b.plain.extend_from_slice(data);
                }
                None => self.current = Some(Block { chunks: vec![idx], plain: data.to_vec(), delta: self.cur_delta.clone() }),
            }
            // a block adopts the dictionary of the first versioned entry it receives; every chunk
            // stored in such a block sits one level deeper in the chain than the dictionary chunks
            if let Some(b) = &mut self.current {
                let adopted = b.delta.is_none() && self.cur_delta.is_some();
                if adopted {
                    b.delta = self.cur_delta.clone();
                }
                if let Some(dict) = &b.delta {
                    let depth = dict.iter().map(|&ci| self.delta_depth[ci as usize]).max().unwrap_or(0).saturating_add(1);
                    if adopted {
                        for &ci in &b.chunks {
                            self.delta_depth[ci as usize] = self.delta_depth[ci as usize].max(depth);
                        }
                    } else {
                        self.delta_depth[idx as usize] = self.delta_depth[idx as usize].max(depth);
                    }
                }
            }
        }
        Ok(idx)
    }

    /// Append an entry and index its name for delta candidate search (views are never candidates).
    fn register_entry(&mut self, entry: Entry) {
        if entry.derived.is_none() {
            if let Some(k) = name_key(&entry.path) {
                self.name_index.entry(k).or_default().push(self.entries.len());
            }
        }
        self.entries.push(entry);
    }

    /// Keep the bytes of recently stored chunks (bounded window) for intra-archive delta dictionaries.
    fn remember(&mut self, idx: u32, data: &[u8]) {
        if !self.opts.delta || self.opts.block_size == 0 {
            return;
        }
        let arc = Arc::new(data.to_vec());
        self.recent_bytes += data.len();
        self.recent.push_back((idx, arc.clone()));
        self.recent_map.insert(idx, arc);
        while self.recent_bytes > DELTA_WINDOW {
            if let Some((old, bytes)) = self.recent.pop_front() {
                self.recent_bytes -= bytes.len();
                self.recent_map.remove(&old);
            } else {
                break;
            }
        }
    }

    /// Register a chunk of a reference archive as external in this archive's chunk table.
    fn intern_external(&mut self, h: Hash, size: u32, ri: usize) -> u32 {
        if let Some(&i) = self.index.get(&h) {
            return i;
        }
        let idx = self.hashes.len() as u32;
        self.hashes.push(h);
        self.sizes.push(size);
        self.delta_depth.push(0);
        self.external.push(1);
        self.index.insert(h, idx);
        self.referenced_chunks += 1;
        self.referenced_bytes += size as u64;
        self.ref_meta[ri].chunks += 1;
        self.ref_meta[ri].bytes += size as u64;
        idx
    }

    /// Candidate earlier versions of `path`, best first: the same path in a reference archive,
    /// then similarly named entries (same extension, common name prefix of at least half the
    /// name) of the reference archives and of this archive; at most three are tried.
    fn delta_candidates(&self, path: &str) -> Vec<DeltaCandidate> {
        let name = file_name(path);
        let ext = extension(path);
        let similar = |other: &str| -> Option<usize> {
            if other == path || extension(other) != ext {
                return None;
            }
            let other_name = file_name(other);
            let lcp = common_prefix(name, other_name);
            (lcp >= 3 && lcp * 2 >= name.len().min(other_name.len())).then_some(lcp)
        };
        let mut ranked: Vec<(usize, usize, DeltaCandidate)> = Vec::new(); // (score, sequence, candidate)
        let key = name_key(path);
        for (ri, r) in self.refs.iter().enumerate() {
            let exact = self.ref_path_index[ri].get(path).copied();
            let bucket = key.as_ref().and_then(|k| self.ref_name_index[ri].get(k)).map(|v| v.as_slice()).unwrap_or(&[]);
            for &ei in exact.iter().chain(bucket.iter()) {
                let e = &r.manifest.entries[ei];
                let score = if e.path == path {
                    usize::MAX
                } else {
                    match similar(&e.path) {
                        Some(l) => l,
                        None => continue,
                    }
                };
                let mut chunks = Vec::new();
                for cj in entry_chunk_indices(e) {
                    let c = &r.index.chunks[cj as usize];
                    if c.x != 0 || c.h.len() != 32 {
                        continue;
                    }
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&c.h);
                    chunks.push((h, c.s));
                }
                if !chunks.is_empty() {
                    ranked.push((score, ranked.len(), DeltaCandidate { source: DictSource::Ref { ri, chunks } }));
                }
            }
        }
        let bucket = key.as_ref().and_then(|k| self.name_index.get(k)).map(|v| v.as_slice()).unwrap_or(&[]);
        for &ei in bucket {
            let e = &self.entries[ei];
            let Some(score) = similar(&e.path) else { continue };
            let mut chunks = Vec::new();
            for ci in entry_chunk_indices(e) {
                // dictionary material: not too deep in a delta chain, bytes at hand (recent window or a reference)
                if self.delta_depth[ci as usize] < MAX_WRITER_DELTA_DEPTH && (self.recent_map.contains_key(&ci) || self.external[ci as usize] != 0) {
                    chunks.push(ci);
                }
            }
            if !chunks.is_empty() {
                ranked.push((score, ranked.len(), DeltaCandidate { source: DictSource::Local { chunks } }));
            }
        }
        // best score first; among equals the most recently added entry wins
        ranked.sort_by(|x, y| (y.0, y.1).cmp(&(x.0, x.1)));
        ranked.truncate(3);
        ranked.into_iter().map(|(_, _, c)| c).collect()
    }

    /// Bytes of one dictionary chunk: the recent window for stored chunks, a reference archive for external ones.
    fn dict_chunk_bytes(&mut self, ci: u32) -> Option<Vec<u8>> {
        if self.external[ci as usize] != 0 {
            let (ri, cj) = *self.ref_lookup.get(&self.hashes[ci as usize])?;
            self.refs[ri].chunk(cj).ok()
        } else {
            self.recent_map.get(&ci).map(|b| b.to_vec())
        }
    }

    /// Confirm a candidate against the bytes about to be stored (at least one shared chunk), then
    /// build the dictionary chunk list; reference chunks are registered as external only now.
    fn confirm_delta(&mut self, cand: DeltaCandidate, parts: &[&[u8]]) -> Option<Vec<u32>> {
        // 1. the candidate's bytes (bounded), which also become the dictionary
        let mut dict = Vec::new();
        let mut out = Vec::new();
        match cand.source {
            DictSource::Ref { ri, chunks } => {
                for (h, size) in chunks {
                    if dict.len() + size as usize > DELTA_DICT_MAX {
                        break;
                    }
                    let (_, cj) = *self.ref_lookup.get(&h)?;
                    let bytes = self.refs[ri].chunk(cj).ok()?;
                    dict.extend_from_slice(&bytes);
                    out.push((h, size));
                }
                // 2. fine-grained fingerprints: at least a quarter of the new bytes must be shared
                if !shares_content(&dict, parts) {
                    return None;
                }
                let idx: Vec<u32> = out.into_iter().map(|(h, size)| self.intern_external(h, size, ri)).collect();
                (!idx.is_empty()).then_some(idx)
            }
            DictSource::Local { chunks } => {
                let mut idx = Vec::new();
                for ci in chunks {
                    let bytes = self.dict_chunk_bytes(ci)?;
                    if dict.len() + bytes.len() > DELTA_DICT_MAX {
                        break;
                    }
                    dict.extend_from_slice(&bytes);
                    idx.push(ci);
                }
                if !shares_content(&dict, parts) {
                    return None;
                }
                (!idx.is_empty()).then_some(idx)
            }
        }
    }

    /// When the whole dictionary already sits in the current block and the incoming entry fits in
    /// it too, the codec's own window covers the earlier version: no delta, no split.
    fn drop_delta_if_base_in_block(&mut self, incoming: usize) {
        if let (Some(d), Some(b)) = (&self.cur_delta, &self.current) {
            // (at least half of the incoming entry must still fit next to its base for the window to matter)
            if b.delta.is_some() || b.plain.len() + incoming / 2 > self.opts.block_size as usize {
                return;
            }
            let (mut inside, mut outside) = (0usize, 0usize);
            for &ci in d {
                let size = self.sizes[ci as usize] as usize;
                if b.chunks.contains(&ci) {
                    inside += size;
                } else {
                    outside += size;
                }
            }
            if outside * 4 < inside + outside {
                self.cur_delta = None;
            }
        }
    }

    /// Bytes of a delta dictionary (None when any chunk is unavailable, then no delta is tried).
    fn delta_dictionary(&mut self, chunks: &[u32]) -> Option<Vec<u8>> {
        let mut dict = Vec::new();
        for &ci in chunks {
            dict.extend_from_slice(&self.dict_chunk_bytes(ci)?);
        }
        Some(dict)
    }

    /// Train the dictionary on the warm-up chunks, then queue them.
    fn finish_warmup(&mut self) -> Result<()> {
        if self.dict_done {
            return Ok(());
        }
        self.dict_done = true;
        let samples: Vec<&[u8]> = self.warmup.iter().map(|(_, d)| d.as_slice()).collect();
        self.dict = codec::train_dict(&samples, self.opts.dict_size);
        let warm = std::mem::take(&mut self.warmup);
        for (idx, data) in warm {
            self.ready.push(Block { chunks: vec![idx], plain: data, delta: None });
            if self.batch_ready() {
                self.flush_ready()?;
            }
        }
        Ok(())
    }

    /// Compress the ready blocks in parallel and write them in order.
    fn flush_ready(&mut self) -> Result<()> {
        if self.ready.is_empty() {
            return Ok(());
        }
        let mut blocks = std::mem::take(&mut self.ready);
        let copts = CodecOptions { level: self.opts.level, long_window_log: 27, dict: self.dict.clone(), choice: self.opts.codec };
        // delta dictionaries are gathered up front (reference archives and the recent window)
        let mut dicts: Vec<Option<Vec<u8>>> = Vec::with_capacity(blocks.len());
        for b in blocks.iter_mut() {
            let d = match &b.delta {
                Some(chunks) => self.delta_dictionary(chunks),
                None => None,
            };
            if d.is_none() {
                b.delta = None;
            }
            dicts.push(d);
        }
        let level = self.opts.level;
        let compressed: Vec<codec::Encoded> = blocks
            .par_iter()
            .zip(dicts.par_iter())
            .map(|(b, d)| {
                let mut e = codec::compress_best(&b.plain, &copts)?;
                if let Some(dict) = d {
                    let z = codec::zstd_compress_with_prefix(&b.plain, level, dict)?;
                    if z.len() < e.bytes.len() {
                        e = codec::Encoded { codec: codec::ZSTD_DELTA, bytes: z, params: Vec::new(), filter: codec::FILTER_NONE };
                    }
                }
                Ok(e)
            })
            .collect::<Result<Vec<_>>>()?;
        for (b, e) in blocks.into_iter().zip(compressed) {
            let c = e.codec;
            *self.codec_hist.entry(codec::name(c).to_string()).or_insert(0) += 1;
            let blob_index = self.blob_records.len() as u64;
            let aad = blob_aad(c, b.plain.len() as u32, b.chunks[0], b.chunks.len() as u32);
            let stored = match &self.key {
                Some(k) => crypto::seal(k, crypto::LABEL_BLOB, blob_index, &aad, &e.bytes)?,
                None => e.bytes,
            };
            match codec::filter_id(e.filter) {
                codec::FILTER_NONE => {}
                codec::FILTER_X86 => *self.codec_hist.entry("filter:x86".to_string()).or_insert(0) += 1,
                codec::FILTER_ARM64 => *self.codec_hist.entry("filter:arm64".to_string()).or_insert(0) += 1,
                _ => *self.codec_hist.entry("filter:other".to_string()).or_insert(0) += 1,
            }
            let d = if c == codec::ZSTD_DELTA { b.delta.unwrap_or_default() } else { Vec::new() };
            self.blob_records.push(BlobRecord { off: self.blobs_written, clen: stored.len() as u32, ulen: b.plain.len() as u32, codec: c, n: b.chunks, p: e.params, f: e.filter, d });
            self.out.write_all(&stored)?;
            self.blobs_written += stored.len() as u64;
            self.pos += stored.len() as u64;
        }
        Ok(())
    }

    fn entry_skeleton(&self, path: String, size: u64, hash: Hash, fs_path: &Path) -> Entry {
        let mtime = if self.opts.keep_mtime {
            std::fs::metadata(fs_path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)
        } else {
            None
        };
        Entry { path, size, h: ByteBuf::from(hash.to_vec()), mode: "raw".into(), chunks: Vec::new(), mtime, zip: None, streams: None, note: None, derived: None }
    }

    fn add_file(&mut self, fs_path: &Path, arcname: &str) -> Result<()> {
        let path = paths::normalize(arcname)?;
        let size = std::fs::metadata(fs_path)?.len();
        if size > self.opts.limits.max_entry {
            return Err(Error::Limit(format!("{path}: entry larger than max_entry")));
        }
        self.input_bytes += size;
        if self.input_bytes > self.opts.limits.max_total {
            return Err(Error::Limit("total input larger than max_total".into()));
        }

        // Delta coding: a candidate earlier version is confirmed on the real bytes before use.
        let candidates = if self.opts.delta && self.opts.block_size > 0 { self.delta_candidates(&path) } else { Vec::new() };
        self.cur_delta = None;

        // Containers need the whole file (preflate), stored views and delta candidates too; everything else streams.
        let is_container = self.opts.container_aware && (container::is_zip_like(&path) || container::is_stream_container(&path)) && size > 64;
        let wants_view = self.opts.canonical && canonical::derivable(&path).is_some();
        if is_container || wants_view || (!candidates.is_empty() && size <= DELTA_WHOLE_FILE_MAX) {
            let data = std::fs::read(fs_path)?;
            if is_container {
                self.add_container(&path, &data, size, fs_path, candidates)?;
            } else {
                for c in candidates {
                    if let Some(d) = self.confirm_delta(c, &[&data]) {
                        self.cur_delta = Some(d);
                        break;
                    }
                }
                self.drop_delta_if_base_in_block(data.len());
                let mut entry = self.entry_skeleton(path.clone(), size, chunk::hash(&data), fs_path);
                entry.chunks = self.chunk_buffer(&data)?;
                self.register_entry(entry);
                self.cur_delta = None;
            }
            if wants_view {
                self.cur_delta = None;
                self.add_view(&path, &data)?;
            }
            return Ok(());
        }

        // Streaming path: chunk the file as it is read, hash the whole file on the fly.
        let file = File::open(fs_path)?;
        let reader = BufReader::with_capacity(1 << 20, file);
        let p = self.opts.chunk;
        let mut hasher = blake3::Hasher::new();
        let mut refs = Vec::new();
        let mut total = 0u64;
        if size > 0 {
            let cdc = StreamCDC::with_level(reader, p.min, p.avg, p.max, Normalization::Level2);
            for item in cdc {
                let c = item.map_err(|e| Error::Io(std::io::Error::other(format!("chunking {}: {e}", fs_path.display()))))?;
                hasher.update(&c.data);
                total += c.data.len() as u64;
                refs.push(self.ingest_chunk(&c.data)?);
            }
        }
        if total != size {
            return Err(Error::Io(std::io::Error::other(format!("{}: size changed while reading", fs_path.display()))));
        }
        let mut entry = self.entry_skeleton(path, size, *hasher.finalize().as_bytes(), fs_path);
        entry.chunks = refs;
        self.register_entry(entry);
        Ok(())
    }

    /// A ZIP/OPC, PDF or JPEG file: explode it into invertible parts when the rebuild is bit-exact,
    /// otherwise store it raw with a note.
    fn add_container(&mut self, path: &str, data: &[u8], size: u64, fs_path: &Path, mut candidates: Vec<DeltaCandidate>) -> Result<()> {
        let mut entry = self.entry_skeleton(path.to_string(), size, chunk::hash(data), fs_path);
        if container::is_zip_like(path) {
            if let Some((mut recipe, parts)) = container::try_bitexact(data) {
                let views: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
                for c in std::mem::take(&mut candidates) {
                    if let Some(d) = self.confirm_delta(c, &views) {
                        self.cur_delta = Some(d);
                        break;
                    }
                }
                self.drop_delta_if_base_in_block(views.iter().map(|v| v.len()).sum());
                for (m, part) in recipe.members.iter_mut().zip(parts.iter()) {
                    m.chunks = self.chunk_buffer(part)?;
                }
                self.cur_delta = None;
                entry.mode = "zip".into();
                entry.zip = Some(recipe);
                self.exploded += 1;
                self.register_entry(entry);
                return Ok(());
            }
            entry.note = Some("container could not be rebuilt bit-exact; stored raw".into());
        } else if let Some((mut recipe, parts)) = container::try_bitexact_streams(data, self.opts.jpeg_recompression) {
            let views: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
            for c in std::mem::take(&mut candidates) {
                if let Some(d) = self.confirm_delta(c, &views) {
                    self.cur_delta = Some(d);
                    break;
                }
            }
            self.drop_delta_if_base_in_block(views.iter().map(|v| v.len()).sum());
            for (s, part) in recipe.segments.iter_mut().zip(parts.iter()) {
                s.chunks = self.chunk_buffer(part)?;
            }
            self.cur_delta = None;
            entry.mode = "streams".into();
            entry.streams = Some(recipe);
            self.exploded += 1;
            self.register_entry(entry);
            return Ok(());
        } else {
            entry.note = Some("no invertible zlib streams found; stored raw".into());
        }
        self.fallback += 1;
        for c in std::mem::take(&mut candidates) {
            if let Some(d) = self.confirm_delta(c, &[data]) {
                self.cur_delta = Some(d);
                break;
            }
        }
        self.drop_delta_if_base_in_block(data.len());
        entry.chunks = self.chunk_buffer(data)?;
        self.cur_delta = None;
        self.register_entry(entry);
        Ok(())
    }

    /// Generate and store the canonical view of a DOCX/PDF entry as a derived entry
    /// `.tsaur/views/<path>.<md|txt>`. A view that cannot be generated is recorded as a note on
    /// the source entry; the original is stored in every case.
    fn add_view(&mut self, source: &str, data: &[u8]) -> Result<()> {
        let Some(kind) = canonical::derivable(source) else { return Ok(()) };
        let text = match canonical::canonical_view(source, data) {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(()),
            Err(e) => {
                if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.path == source) {
                    entry.note = Some(format!("canonical view not generated: {e}"));
                }
                return Ok(());
            }
        };
        let vpath = paths::normalize(&canonical::view_path(source, kind))?;
        let bytes = text.into_bytes();
        let derived = Derived { from: source.to_string(), view: kind.to_string(), generator: crate::WRITER.to_string(), tokens_est: (bytes.len() as f64 / 3.5).ceil() as u64 };
        let mut entry = Entry {
            path: vpath,
            size: bytes.len() as u64,
            h: ByteBuf::from(chunk::hash(&bytes).to_vec()),
            mode: "raw".into(),
            chunks: Vec::new(),
            mtime: None,
            zip: None,
            streams: None,
            note: None,
            derived: Some(derived),
        };
        entry.chunks = self.chunk_buffer(&bytes)?;
        self.register_entry(entry);
        self.views += 1;
        Ok(())
    }

    fn chunk_buffer(&mut self, data: &[u8]) -> Result<Vec<u32>> {
        let mut refs = Vec::new();
        for (off, len) in chunk::boundaries(data, &self.opts.chunk) {
            refs.push(self.ingest_chunk(&data[off..off + len])?);
        }
        Ok(refs)
    }

    fn finish(mut self) -> Result<PackReport> {
        // flush everything that is still queued
        self.finish_warmup()?;
        if let Some(b) = self.current.take() {
            self.ready.push(b);
        }
        self.flush_ready()?;
        let opts = self.opts.clone();

        // blobs section (contiguous, already written)
        let blobs_len = self.blobs_written;
        let blobs_off = self.blobs_off;
        let blobs_hash = self.hash_written_range(blobs_off, blobs_len)?;
        self.sections.push(SectionEntry { t: section::BLOBS, off: blobs_off, len: blobs_len, h: ByteBuf::from(blobs_hash.to_vec()) });

        let mut dict_bytes = 0usize;
        if let Some(d) = self.dict.clone() {
            dict_bytes = d.len();
            let bytes = match &self.key {
                Some(k) => crypto::seal(k, crypto::LABEL_DICT, 0, b"dict", &d)?,
                None => d,
            };
            self.write_section(section::DICTIONARY, &bytes)?;
        }

        let index = ChunkIndex {
            chunks: self.hashes.iter().zip(self.sizes.iter()).zip(self.external.iter()).map(|((h, s), x)| ChunkRef { h: ByteBuf::from(h.to_vec()), s: *s, x: *x }).collect(),
            blobs: std::mem::take(&mut self.blob_records),
        };
        let index_bytes = seal_section(&self.key, crypto::LABEL_INDEX, b"index", &zstd_wrap(&format::cbor_encode(&index)?)?)?;
        self.write_section(section::CHUNK_INDEX, &index_bytes)?;

        let root = chunk::merkle_root(&self.hashes);
        let entries = std::mem::take(&mut self.entries);
        let n_entries = entries.len();
        let manifest = Manifest {
            tsaur: 1,
            profile: opts.profile.clone(),
            fidelity: if opts.canonical { "hybrid".into() } else { "bit-exact".into() },
            chunking: opts.chunk,
            solid_block: opts.block_size,
            limits: opts.limits,
            root: ByteBuf::from(root.to_vec()),
            entries,
            created: if opts.timestamp { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()) } else { None },
            generator: crate::WRITER.to_string(),
            refs: std::mem::take(&mut self.ref_meta),
        };
        let manifest_bytes = seal_section(&self.key, crypto::LABEL_MANIFEST, b"manifest", &zstd_wrap(&format::cbor_encode(&manifest)?)?)?;
        self.write_section(section::MANIFEST, &manifest_bytes)?;

        if let Some(seed) = &opts.sign_seed {
            let msg = format::signing_message(&self.header, &self.sections);
            let block = SignatureBlock { alg: "ed25519".into(), key: ByteBuf::from(crypto::sig::public_key(seed).to_vec()), sig: ByteBuf::from(crypto::sig::sign(seed, &msg).to_vec()) };
            let bytes = format::cbor_encode(&block)?;
            self.write_section(section::SIGNATURES, &bytes)?;
        }

        let table = format::cbor_encode(&self.sections)?;
        let table_off = self.pos;
        self.out.write_all(&table)?;
        self.out.write_all(&format::encode_trailer(table_off, table.len() as u64, format::crc32(&table)))?;
        self.out.flush()?;
        let archive_bytes = table_off + table.len() as u64 + format::TRAILER_LEN as u64;
        let blobs = index.blobs.len();

        Ok(PackReport {
            skipped_links: 0,
            archive_bytes,
            input_bytes: self.input_bytes,
            logical_bytes: self.logical_bytes,
            unique_bytes: self.unique_bytes,
            chunks_total: self.chunk_refs,
            chunks_unique: self.hashes.len(),
            blobs,
            dict_bytes,
            codec_hist: self.codec_hist.clone(),
            entries: n_entries,
            containers_exploded: self.exploded,
            containers_fallback: self.fallback,
            root: hex::encode(root),
            encrypted: self.key.is_some(),
            signed: opts.sign_seed.is_some(),
            referenced_chunks: self.referenced_chunks,
            referenced_bytes: self.referenced_bytes,
            views: self.views,
        })
    }

    /// Hash a range already written to the output (re-read from disk, streaming).
    fn hash_written_range(&mut self, off: u64, len: u64) -> Result<Hash> {
        use std::io::{Read, Seek, SeekFrom};
        self.out.flush()?;
        let mut f = self.out.get_ref().try_clone()?;
        f.seek(SeekFrom::Start(off))?;
        let mut hasher = blake3::Hasher::new();
        let mut remaining = len;
        let mut buf = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                return Err(Error::Io(std::io::Error::other("short read while hashing output")));
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        Ok(*hasher.finalize().as_bytes())
    }
}

pub(crate) fn blob_aad(codec: u8, ulen: u32, first: u32, count: u32) -> [u8; 13] {
    let mut a = [0u8; 13];
    a[0] = codec;
    a[1..5].copy_from_slice(&ulen.to_le_bytes());
    a[5..9].copy_from_slice(&first.to_le_bytes());
    a[9..13].copy_from_slice(&count.to_le_bytes());
    a
}

/// Sections that hold structured data are zstd-compressed (level 19) before sealing.
pub(crate) fn zstd_wrap(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut c = zstd::bulk::Compressor::new(19)?;
    let mut out = (bytes.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&c.compress(bytes)?);
    Ok(out)
}

pub(crate) fn zstd_unwrap(bytes: &[u8], max: usize) -> Result<Vec<u8>> {
    if bytes.len() < 4 {
        return Err(Error::Corrupt("section too short".into()));
    }
    let ulen = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if ulen > max {
        return Err(Error::Limit(format!("section declares {ulen} bytes, limit {max}")));
    }
    let mut d = zstd::bulk::Decompressor::new()?;
    let out = d.decompress(&bytes[4..], ulen)?;
    if out.len() != ulen {
        return Err(Error::Corrupt("section length mismatch".into()));
    }
    Ok(out)
}

fn seal_section(key: &Option<ArchiveKey>, label: u8, aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    match key {
        Some(k) => crypto::seal(k, label, 0, aad, bytes),
        None => Ok(bytes.to_vec()),
    }
}

/// (filesystem path, archive name) pairs, sorted by archive name.
pub type Inputs = Vec<(PathBuf, String)>;

/// Collect (filesystem path, archive name) pairs from files and directories, sorted by name.
pub fn collect_inputs(inputs: &[PathBuf]) -> Result<Inputs> {
    Ok(collect_inputs_noting(inputs)?.0)
}

/// Like `collect_inputs`, also returning the symbolic links that were skipped. Links found
/// inside a directory are never followed: their targets may lie outside the inputs and a
/// directory link can loop. An input named explicitly on the command line is followed, because
/// that is what the caller asked for. Links are never recreated on extraction (`docs/V1-CONTRACT.md`).
pub fn collect_inputs_noting(inputs: &[PathBuf]) -> Result<(Inputs, Vec<PathBuf>)> {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let mut stack = vec![input.clone()];
            while let Some(dir) = stack.pop() {
                let mut children: Vec<_> = std::fs::read_dir(&dir)?.collect::<std::io::Result<Vec<_>>>()?;
                children.sort_by_key(|e| e.file_name());
                for e in children {
                    let p = e.path();
                    let kind = e.file_type()?;
                    if kind.is_symlink() {
                        skipped.push(p);
                    } else if kind.is_dir() {
                        stack.push(p);
                    } else if kind.is_file() {
                        let rel = p.strip_prefix(input).map_err(|_| Error::Invalid("path prefix".into()))?;
                        out.push((p.clone(), rel.to_string_lossy().replace('\\', "/")));
                    }
                }
            }
        } else if input.is_file() {
            let name = input.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            out.push((input.clone(), name));
        } else {
            return Err(Error::Missing(format!("input not found: {}", input.display())));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    Ok((out, skipped))
}

/// Pack `inputs` (files or directories) into `out_path`.
pub fn pack(inputs: &[PathBuf], out_path: &Path, opts: PackOptions) -> Result<PackReport> {
    let (files, skipped) = collect_inputs_noting(inputs)?;
    if files.is_empty() {
        return Err(Error::Missing("nothing to pack".into()));
    }
    let result = (|| {
        let mut p = Packer::new(opts, out_path)?;
        for (fs_path, arcname) in &files {
            p.add_file(fs_path, arcname)?;
        }
        p.finish()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(out_path);
    }
    result.map(|mut r| {
        r.skipped_links = skipped.len();
        r
    })
}
