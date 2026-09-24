//! Offline volume sets: one archive file striped across `N` data volumes plus `M` parity volumes
//! (systematic Reed-Solomon over GF(2^8), one piece per volume per stripe), so that the archive
//! can be spread over directories or removable drives and rebuilt from any `N` of the `N + M`
//! volumes. Every volume carries the complete descriptor (geometry, archive hash, every piece
//! hash), so no separate index file is needed. See `docs/design/VOLUME-SETS.md`.
//!
//! Everything streams one stripe at a time: memory stays at `(N + M) × piece_size` whatever the
//! archive size. The archive itself is treated as opaque bytes; `join` verifies the rebuilt file
//! against the archive hash recorded at split time before giving it its final name.

use crate::chunk::{self, Hash};
use crate::error::{Error, Result};
use crate::format;
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const VOLUME_MAGIC: [u8; 4] = *b"TSV\x1A";
pub const TRAILER_MAGIC: [u8; 4] = *b"VST\x1A";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 64;
pub const TRAILER_LEN: usize = 56;
pub const MIN_PIECE_SIZE: u32 = 64 << 10;
pub const MAX_PIECE_SIZE: u32 = 64 << 20;
pub const DEFAULT_PIECE_SIZE: u32 = 1 << 20;
pub const MAX_VOLUMES: usize = 256;
/// Descriptors above this size are refused before allocation.
pub const MAX_DESCRIPTOR: u64 = 64 << 20;
/// Most data pieces a set may have (bounds descriptor size, memory and index arithmetic); larger
/// archives need a larger piece size.
pub const MAX_PIECES: u32 = 2_000_000;
/// Longest archive name accepted in a descriptor.
pub const MAX_ARCHIVE_NAME: usize = 255;
pub const EXTENSION: &str = "tsrv";

/// The descriptor stored (identically) in every volume of a set.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeSet {
    pub v: u16,
    pub set_id: ByteBuf,
    pub archive_name: String,
    pub archive_size: u64,
    /// BLAKE3 of the whole archive file: `join` verifies the rebuilt file against it.
    pub archive_b3: ByteBuf,
    pub piece_size: u32,
    pub data: u16,
    pub parity: u16,
    /// Number of real data pieces (`ceil(archive_size / piece_size)`).
    pub pieces: u32,
    /// `ceil(pieces / data)`.
    pub stripes: u32,
    pub piece_hashes: Vec<ByteBuf>,
    /// `stripes * parity` hashes, index `stripe * parity + j`.
    pub parity_hashes: Vec<ByteBuf>,
    /// Merkle root over the data piece hashes.
    pub root: ByteBuf,
    pub generator: String,
}

impl VolumeSet {
    pub fn total(&self) -> usize {
        self.data as usize + self.parity as usize
    }

    pub fn is_parity(&self, index: usize) -> bool {
        index >= self.data as usize
    }

    /// Number of pieces stored on volume `index`.
    pub fn volume_pieces(&self, index: usize) -> u64 {
        if self.is_parity(index) {
            self.stripes as u64
        } else {
            let (p, n) = (self.pieces as u64, self.data as u64);
            let v = index as u64;
            if v >= p {
                0
            } else {
                (p - v).div_ceil(n)
            }
        }
    }

    pub fn payload_len(&self, index: usize) -> u64 {
        self.volume_pieces(index) * self.piece_size as u64
    }

    /// File name of volume `index` (1-based in the name, data volumes first). The archive name
    /// comes from the descriptor, which may have arrived from another machine: it is used only
    /// when it is a safe single path component (no separators, colons, control characters,
    /// reserved device names or trailing dots/spaces); otherwise the set id names the files.
    pub fn volume_name(&self, index: usize) -> String {
        let width = if self.total() > 99 { 3 } else { 2 };
        let stem = match crate::paths::normalize(&self.archive_name) {
            Ok(n) if !n.contains('/') && n.len() <= MAX_ARCHIVE_NAME => n,
            _ => format!("set-{}", hex::encode(&self.set_id[..8])),
        };
        format!("{stem}.v{:0width$}.{EXTENSION}", index + 1, width = width)
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Error::Corrupt(format!("volume descriptor: {m}"));
        if self.v != VERSION {
            return Err(Error::Version(self.v));
        }
        if self.data == 0 || self.total() > MAX_VOLUMES {
            return Err(bad("volume counts out of range"));
        }
        if !(MIN_PIECE_SIZE..=MAX_PIECE_SIZE).contains(&self.piece_size) {
            return Err(bad("piece size out of range"));
        }
        if self.set_id.len() != 32 || self.archive_b3.len() != 32 || self.root.len() != 32 {
            return Err(bad("hash lengths"));
        }
        let pieces = self.archive_size.div_ceil(self.piece_size as u64);
        if pieces > MAX_PIECES as u64 {
            return Err(Error::Limit(format!("volume descriptor: {pieces} pieces exceed the limit of {MAX_PIECES}")));
        }
        if pieces != self.pieces as u64 || pieces.div_ceil(self.data as u64) != self.stripes as u64 {
            return Err(bad("piece or stripe count does not match the archive size"));
        }
        if self.piece_hashes.len() != self.pieces as usize || self.parity_hashes.len() != self.stripes as usize * self.parity as usize {
            return Err(bad("hash list lengths"));
        }
        if self.piece_hashes.iter().chain(self.parity_hashes.iter()).any(|h| h.len() != 32) {
            return Err(bad("hash length"));
        }
        if self.archive_name.is_empty() || self.archive_name.len() > MAX_ARCHIVE_NAME || self.archive_name.contains(['/', '\\']) || self.archive_name.chars().any(|c| c.is_control()) {
            return Err(bad("archive name"));
        }
        Ok(())
    }

    pub(crate) fn piece_hash(&self, index: u32) -> [u8; 32] {
        let mut h = [0u8; 32];
        h.copy_from_slice(&self.piece_hashes[index as usize]);
        h
    }

    pub(crate) fn parity_hash(&self, stripe: u32, j: usize) -> [u8; 32] {
        let mut h = [0u8; 32];
        h.copy_from_slice(&self.parity_hashes[stripe as usize * self.parity as usize + j]);
        h
    }
}

/// The fixed header at the start of every volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeHeader {
    pub index: u16,
    pub data: u16,
    pub parity: u16,
    pub piece_size: u32,
    pub archive_size: u64,
    pub set_id: [u8; 32],
    pub payload_len: u64,
}

pub(crate) fn encode_header(h: &VolumeHeader) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..4].copy_from_slice(&VOLUME_MAGIC);
    b[4..6].copy_from_slice(&VERSION.to_le_bytes());
    b[6..8].copy_from_slice(&h.index.to_le_bytes());
    b[8..10].copy_from_slice(&h.data.to_le_bytes());
    b[10..12].copy_from_slice(&h.parity.to_le_bytes());
    b[12..16].copy_from_slice(&h.piece_size.to_le_bytes());
    b[16..24].copy_from_slice(&h.archive_size.to_le_bytes());
    b[24..56].copy_from_slice(&h.set_id);
    b[56..64].copy_from_slice(&h.payload_len.to_le_bytes());
    b
}

pub(crate) fn decode_header(b: &[u8]) -> Result<VolumeHeader> {
    if b.len() < HEADER_LEN || b[0..4] != VOLUME_MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([b[4], b[5]]);
    if version != VERSION {
        return Err(Error::Version(version));
    }
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&b[24..56]);
    Ok(VolumeHeader {
        index: u16::from_le_bytes([b[6], b[7]]),
        data: u16::from_le_bytes([b[8], b[9]]),
        parity: u16::from_le_bytes([b[10], b[11]]),
        piece_size: u32::from_le_bytes(b[12..16].try_into().unwrap()),
        archive_size: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        set_id,
        payload_len: u64::from_le_bytes(b[56..64].try_into().unwrap()),
    })
}

pub(crate) fn encode_trailer(desc_off: u64, desc_len: u64, desc_hash: &Hash, index: u16) -> [u8; TRAILER_LEN] {
    let mut b = [0u8; TRAILER_LEN];
    b[0..8].copy_from_slice(&desc_off.to_le_bytes());
    b[8..16].copy_from_slice(&desc_len.to_le_bytes());
    b[16..48].copy_from_slice(desc_hash);
    b[48..50].copy_from_slice(&index.to_le_bytes());
    b[52..56].copy_from_slice(&TRAILER_MAGIC);
    b
}

/// (descriptor offset, descriptor length, descriptor hash, volume index)
fn decode_trailer(b: &[u8]) -> Result<(u64, u64, Hash, u16)> {
    if b.len() != TRAILER_LEN || b[52..56] != TRAILER_MAGIC {
        return Err(Error::Corrupt("volume trailer".into()));
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(&b[16..48]);
    Ok((u64::from_le_bytes(b[0..8].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap()), h, u16::from_le_bytes([b[48], b[49]])))
}

pub(crate) fn set_id_for(archive_b3: &Hash, archive_size: u64, piece_size: u32, data: u16, parity: u16) -> Hash {
    let mut h = blake3::Hasher::new_derive_key("tsaur volume set v1");
    h.update(archive_b3);
    h.update(&archive_size.to_le_bytes());
    h.update(&piece_size.to_le_bytes());
    h.update(&data.to_le_bytes());
    h.update(&parity.to_le_bytes());
    *h.finalize().as_bytes()
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let n = r.read(&mut buf[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok(got)
}

fn hash_file(path: &Path) -> Result<(Hash, u64)> {
    let mut f = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((*hasher.finalize().as_bytes(), total))
}

fn rs_new(k: usize, m: usize) -> Result<Option<ReedSolomon>> {
    if m == 0 {
        return Ok(None);
    }
    ReedSolomon::new(k, m).map(Some).map_err(|e| Error::Invalid(format!("reed-solomon: {e:?}")))
}

// ---------------------------------------------------------------- split

pub struct SplitOptions {
    pub data: u16,
    pub parity: u16,
    /// Piece size in bytes; `None` selects `adaptive_piece_size` for the archive and geometry.
    pub piece_size: Option<u32>,
    /// Directories that receive the volumes, round-robin, data volumes first.
    pub outputs: Vec<PathBuf>,
}

/// Target number of pieces per data volume for the adaptive default.
pub const ADAPTIVE_PIECES_PER_VOLUME: u64 = 16;
/// Largest adaptive piece size unless the descriptor would otherwise exceed `ADAPTIVE_MAX_PIECES`.
pub const ADAPTIVE_MAX_PIECE_SIZE: u32 = 1 << 20;
/// Above this many pieces the adaptive rule grows the piece size (descriptor size and memory).
pub const ADAPTIVE_MAX_PIECES: u64 = 1_000_000;

/// Default piece size for an archive of `archive_size` bytes split over `data` data volumes: the
/// smallest power of two between 64 KiB and 1 MiB that gives every data volume at least 16
/// pieces (so padding stays a few percent even for small archives), grown beyond 1 MiB only
/// when the archive would otherwise need more than a million pieces. The chosen size is written
/// into every volume, so readers never depend on this rule.
pub fn adaptive_piece_size(archive_size: u64, data: u16) -> u32 {
    let per_volume = archive_size.div_ceil(ADAPTIVE_PIECES_PER_VOLUME * data.max(1) as u64);
    let mut size = per_volume.max(MIN_PIECE_SIZE as u64).next_power_of_two().min(ADAPTIVE_MAX_PIECE_SIZE as u64);
    while archive_size.div_ceil(size) > ADAPTIVE_MAX_PIECES && size < MAX_PIECE_SIZE as u64 {
        size *= 2;
    }
    size as u32
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumeInfo {
    pub index: usize,
    pub kind: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SplitReport {
    pub set_id: String,
    /// BLAKE3 of the descriptor: authenticates every piece hash (see docs/design/VOLUME-TRUST.md).
    pub descriptor_b3: String,
    pub archive_b3: String,
    pub archive_size: u64,
    pub piece_size: u32,
    pub data: u16,
    pub parity: u16,
    pub pieces: u32,
    pub stripes: u32,
    /// The piece size came from `adaptive_piece_size` rather than the caller.
    pub piece_size_adaptive: bool,
    pub volumes: Vec<VolumeInfo>,
    /// Largest number of volumes that share one output location.
    pub max_per_location: usize,
    /// True when losing the most loaded location would exceed the parity.
    pub placement_warning: bool,
}

/// Where volume `index` of `total` goes when `outputs` locations are given round-robin.
pub fn placement(outputs: &[PathBuf], index: usize) -> &Path {
    &outputs[index % outputs.len()]
}

pub fn split(archive: &Path, opts: &SplitOptions) -> Result<SplitReport> {
    let (n, m) = (opts.data as usize, opts.parity as usize);
    if n == 0 || n + m > MAX_VOLUMES {
        return Err(Error::Invalid(format!("data volumes must be 1..{MAX_VOLUMES} and data + parity at most {MAX_VOLUMES}")));
    }
    if let Some(ps) = opts.piece_size {
        if !(MIN_PIECE_SIZE..=MAX_PIECE_SIZE).contains(&ps) {
            return Err(Error::Invalid(format!("piece size must be between {MIN_PIECE_SIZE} and {MAX_PIECE_SIZE} bytes")));
        }
    }
    if opts.outputs.is_empty() {
        return Err(Error::Invalid("at least one output directory is required".into()));
    }
    for d in &opts.outputs {
        if !d.is_dir() {
            return Err(Error::Missing(format!("output directory {} does not exist", d.display())));
        }
    }
    let archive_name = archive.file_name().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| Error::Invalid("archive path has no file name".into()))?;
    let (archive_b3, archive_size) = hash_file(archive)?;
    let piece_size = opts.piece_size.unwrap_or_else(|| adaptive_piece_size(archive_size, opts.data));
    let ps = piece_size as usize;
    let pieces_u64 = archive_size.div_ceil(ps as u64);
    let pieces = u32::try_from(pieces_u64).ok().filter(|&p| p <= MAX_PIECES).ok_or_else(|| Error::Limit(format!("{pieces_u64} pieces exceed the limit of {MAX_PIECES}: use a larger piece size")))?;
    let stripes = pieces_u64.div_ceil(n as u64) as u32;
    let set_id = set_id_for(&archive_b3, archive_size, piece_size, opts.data, opts.parity);
    let mut set = VolumeSet {
        v: VERSION,
        set_id: ByteBuf::from(set_id.to_vec()),
        archive_name,
        archive_size,
        archive_b3: ByteBuf::from(archive_b3.to_vec()),
        piece_size,
        data: opts.data,
        parity: opts.parity,
        pieces,
        stripes,
        piece_hashes: Vec::with_capacity(pieces as usize),
        parity_hashes: Vec::with_capacity(stripes as usize * m),
        root: ByteBuf::from(vec![0u8; 32]),
        generator: crate::WRITER.to_string(),
    };
    if MAX_DESCRIPTOR < 32 * (pieces as u64 + stripes as u64 * m as u64) {
        return Err(Error::Limit("descriptor too large: use a larger piece size".into()));
    }

    // volume files with their headers
    let mut writers: Vec<BufWriter<File>> = Vec::with_capacity(n + m);
    let mut paths = Vec::with_capacity(n + m);
    for vi in 0..n + m {
        let path = placement(&opts.outputs, vi).join(set.volume_name(vi));
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        let header = VolumeHeader { index: vi as u16, data: opts.data, parity: opts.parity, piece_size, archive_size, set_id, payload_len: set.payload_len(vi) };
        w.write_all(&encode_header(&header))?;
        writers.push(w);
        paths.push(path);
    }

    // stripe by stripe: hash the data pieces, encode parity, append to the volumes
    let rs = rs_new(n, m)?;
    let mut input = BufReader::with_capacity(1 << 20, File::open(archive)?);
    let mut hashes: Vec<Hash> = Vec::with_capacity(pieces as usize);
    let mut shards: Vec<Vec<u8>> = (0..n + m).map(|_| vec![0u8; ps]).collect();
    let mut read_total = 0u64;
    for s in 0..stripes {
        for v in 0..n {
            let pi = s as u64 * n as u64 + v as u64;
            let shard = &mut shards[v];
            if pi < pieces as u64 {
                let got = read_full(&mut input, shard)?;
                shard[got..].fill(0);
                read_total += got as u64;
                hashes.push(chunk::hash(shard));
                writers[v].write_all(shard)?;
            } else {
                shard.fill(0); // virtual piece beyond the end of the archive
            }
        }
        if let Some(rs) = &rs {
            rs.encode(&mut shards).map_err(|e| Error::Invalid(format!("reed-solomon encode: {e:?}")))?;
            for j in 0..m {
                set.parity_hashes.push(ByteBuf::from(chunk::hash(&shards[n + j]).to_vec()));
                writers[n + j].write_all(&shards[n + j])?;
            }
        }
    }
    if read_total != archive_size {
        return Err(Error::Io(std::io::Error::other("archive changed size while it was being split")));
    }
    set.root = ByteBuf::from(chunk::merkle_root(&hashes).to_vec());
    set.piece_hashes = hashes.iter().map(|h| ByteBuf::from(h.to_vec())).collect();
    set.validate()?;

    // descriptor + trailer on every volume
    let desc = format::cbor_encode(&set)?;
    let desc_hash = chunk::hash(&desc);
    let mut volumes = Vec::with_capacity(n + m);
    for (vi, mut w) in writers.into_iter().enumerate() {
        let desc_off = HEADER_LEN as u64 + set.payload_len(vi);
        w.write_all(&desc)?;
        w.write_all(&encode_trailer(desc_off, desc.len() as u64, &desc_hash, vi as u16))?;
        w.flush()?;
        let bytes = desc_off + desc.len() as u64 + TRAILER_LEN as u64;
        volumes.push(VolumeInfo { index: vi, kind: if vi < n { "data" } else { "parity" }, path: paths[vi].clone(), bytes });
    }
    let mut per_location: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for vi in 0..n + m {
        *per_location.entry(placement(&opts.outputs, vi).to_path_buf()).or_insert(0) += 1;
    }
    let max_per_location = per_location.values().copied().max().unwrap_or(0);
    Ok(SplitReport {
        set_id: hex::encode(set_id),
        descriptor_b3: hex::encode(desc_hash),
        archive_b3: hex::encode(archive_b3),
        archive_size,
        piece_size,
        piece_size_adaptive: opts.piece_size.is_none(),
        data: opts.data,
        parity: opts.parity,
        pieces,
        stripes,
        volumes,
        max_per_location,
        placement_warning: max_per_location > m,
    })
}

// ---------------------------------------------------------------- inspect

/// One volume file as found on disk.
#[derive(Clone, Debug, Serialize)]
pub struct FoundVolume {
    pub path: PathBuf,
    pub index: usize,
    pub kind: &'static str,
    pub header_ok: bool,
    pub descriptor_ok: bool,
    /// Payload length matches the geometry.
    pub size_ok: bool,
    /// Indices (stripe positions) of pieces whose hash does not match; filled by verification.
    pub bad_pieces: Vec<u32>,
    pub verified: bool,
}

impl FoundVolume {
    pub fn usable(&self) -> bool {
        self.size_ok && self.bad_pieces.is_empty()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SetStatus {
    pub set_id: String,
    /// BLAKE3 of the descriptor shared by every volume of the set.
    pub descriptor_b3: String,
    pub archive_name: String,
    pub archive_size: u64,
    pub archive_b3: String,
    pub piece_size: u32,
    pub data: u16,
    pub parity: u16,
    pub pieces: u32,
    pub stripes: u32,
    /// By volume index; `None` when the volume was not found.
    pub volumes: Vec<Option<FoundVolume>>,
    pub missing: Vec<usize>,
    /// Stripes that cannot be rebuilt with the volumes at hand (after verification when requested).
    pub stripes_short: u32,
    pub reconstructible: bool,
    /// Files that belong to the set but could not be placed (duplicate index, damaged at both ends).
    pub extra: Vec<(PathBuf, String)>,
    #[serde(skip)]
    pub set: VolumeSet,
}

struct Probe {
    path: PathBuf,
    header: Option<VolumeHeader>,
    trailer_index: Option<u16>,
    set_id: Option<[u8; 32]>,
    descriptor: Option<VolumeSet>,
    file_len: u64,
    error: Option<String>,
}

fn probe(path: &Path) -> Probe {
    let mut p = Probe { path: path.to_path_buf(), header: None, trailer_index: None, set_id: None, descriptor: None, file_len: 0, error: None };
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            p.error = Some(e.to_string());
            return p;
        }
    };
    p.file_len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut hb = [0u8; HEADER_LEN];
    if read_full(&mut f, &mut hb).map(|n| n == HEADER_LEN).unwrap_or(false) {
        match decode_header(&hb) {
            Ok(h) => {
                p.set_id = Some(h.set_id);
                p.header = Some(h);
            }
            // an intact header of another format version is not damage: the volume is refused,
            // never read with the v1 rules through its trailer
            Err(Error::Version(v)) => {
                p.error = Some(format!("volume format version {v} is not supported by this build (version {VERSION})"));
                return p;
            }
            Err(_) => {}
        }
    }
    if p.file_len >= (HEADER_LEN + TRAILER_LEN) as u64 {
        let mut tb = [0u8; TRAILER_LEN];
        let ok = f.seek(SeekFrom::Start(p.file_len - TRAILER_LEN as u64)).is_ok() && read_full(&mut f, &mut tb).map(|n| n == TRAILER_LEN).unwrap_or(false);
        if ok {
            if let Ok((off, len, hash, index)) = decode_trailer(&tb) {
                p.trailer_index = Some(index);
                if len <= MAX_DESCRIPTOR && off.checked_add(len).is_some_and(|e| e + TRAILER_LEN as u64 == p.file_len) {
                    let mut db = vec![0u8; len as usize];
                    if f.seek(SeekFrom::Start(off)).is_ok() && read_full(&mut f, &mut db).map(|n| n as u64 == len).unwrap_or(false) && chunk::hash(&db) == hash {
                        if let Ok(set) = format::cbor_decode::<VolumeSet>(&db) {
                            if set.validate().is_ok() {
                                let mut id = [0u8; 32];
                                id.copy_from_slice(&set.set_id);
                                // the hash-verified descriptor wins over a header that disagrees
                                if p.header.is_some_and(|h| h.set_id != id) {
                                    p.header = None;
                                }
                                p.set_id = Some(id);
                                p.descriptor = Some(set);
                            }
                        }
                    }
                }
            }
        }
    }
    if p.header.is_none() && p.descriptor.is_none() {
        p.error = Some("neither the header nor the trailer of this file is a valid T-saur volume".into());
    }
    p
}

/// Expand directories to the `.tsrv` files they contain (non-recursive), keep files as given.
pub fn expand(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(p)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|f| f.is_file() && f.extension().is_some_and(|x| x == EXTENSION)).collect();
            found.sort();
            out.extend(found);
        } else {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// Group the given files/directories into volume sets and, optionally, verify every piece.
pub fn inspect(paths: &[PathBuf], verify: bool) -> Result<Vec<SetStatus>> {
    let files = expand(paths)?;
    let probes: Vec<Probe> = files.iter().map(|f| probe(f)).collect();
    let mut groups: BTreeMap<[u8; 32], Vec<usize>> = BTreeMap::new();
    let mut unknown: Vec<(PathBuf, String)> = Vec::new();
    for (i, p) in probes.iter().enumerate() {
        match p.set_id {
            Some(id) => groups.entry(id).or_default().push(i),
            None => unknown.push((p.path.clone(), p.error.clone().unwrap_or_else(|| "unrecognised".into()))),
        }
    }
    let mut out = Vec::new();
    for (id, members) in groups {
        // one descriptor for the set: any member whose trailer verified
        let descriptor_b3 = members.iter().find_map(|&i| probes[i].descriptor.as_ref().map(|d| hex::encode(chunk::hash(&format::cbor_encode(d).unwrap_or_default())))).unwrap_or_default();
        let Some(set) = members.iter().find_map(|&i| probes[i].descriptor.clone()) else {
            for &i in &members {
                unknown.push((probes[i].path.clone(), "no member of this set has an intact descriptor".into()));
            }
            continue;
        };
        let mut volumes: Vec<Option<FoundVolume>> = vec![None; set.total()];
        let mut extra = Vec::new();
        for &i in &members {
            let p = &probes[i];
            let index = match (p.header.map(|h| h.index as usize), p.trailer_index.map(|t| t as usize)) {
                (Some(h), Some(t)) if h != t => {
                    extra.push((p.path.clone(), "header and trailer disagree on the volume index".into()));
                    continue;
                }
                (Some(h), _) => h,
                (None, Some(t)) => t,
                (None, None) => {
                    extra.push((p.path.clone(), "volume index unknown".into()));
                    continue;
                }
            };
            if index >= set.total() {
                extra.push((p.path.clone(), format!("volume index {index} out of range")));
                continue;
            }
            let header_ok = p
                .header
                .is_some_and(|h| h.data == set.data && h.parity == set.parity && h.piece_size == set.piece_size && h.archive_size == set.archive_size && h.payload_len == set.payload_len(index));
            let expected_len = HEADER_LEN as u64 + set.payload_len(index);
            let size_ok = p.file_len >= expected_len + TRAILER_LEN as u64;
            let mut fv = FoundVolume {
                path: p.path.clone(),
                index,
                kind: if set.is_parity(index) { "parity" } else { "data" },
                header_ok,
                descriptor_ok: p.descriptor.is_some(),
                size_ok,
                bad_pieces: Vec::new(),
                verified: false,
            };
            if verify && size_ok {
                fv.bad_pieces = verify_volume(&set, &p.path, index)?;
                fv.verified = true;
            }
            match &volumes[index] {
                Some(existing) if existing.usable() || !fv.usable() => extra.push((p.path.clone(), format!("duplicate of volume {}", index + 1))),
                _ => volumes[index] = Some(fv),
            }
        }
        let missing: Vec<usize> = (0..set.total()).filter(|&i| volumes[i].is_none()).collect();
        let stripes_short = short_stripes(&set, &volumes);
        out.push(SetStatus {
            set_id: hex::encode(id),
            descriptor_b3,
            archive_name: set.archive_name.clone(),
            archive_size: set.archive_size,
            archive_b3: hex::encode(&set.archive_b3),
            piece_size: set.piece_size,
            data: set.data,
            parity: set.parity,
            pieces: set.pieces,
            stripes: set.stripes,
            volumes,
            missing,
            stripes_short,
            reconstructible: stripes_short == 0,
            extra,
            set,
        });
    }
    if out.is_empty() {
        let detail = unknown.iter().map(|(p, e)| format!("{}: {e}", p.display())).collect::<Vec<_>>().join("; ");
        return Err(Error::Missing(if detail.is_empty() { "no volume files found".into() } else { format!("no usable volume set among the inputs ({detail})") }));
    }
    // unrelated files are reported on every set (they belong to none)
    for s in &mut out {
        s.extra.extend(unknown.iter().cloned());
    }
    Ok(out)
}

/// Stripes that have fewer than `data` intact pieces (virtual pieces count as intact).
fn short_stripes(set: &VolumeSet, volumes: &[Option<FoundVolume>]) -> u32 {
    let n = set.data as usize;
    let mut short = 0u32;
    for s in 0..set.stripes {
        let mut available = 0usize;
        for (vi, vol) in volumes.iter().enumerate() {
            let virtual_piece = !set.is_parity(vi) && (s as u64 * n as u64 + vi as u64) >= set.pieces as u64;
            if virtual_piece {
                available += 1;
                continue;
            }
            if let Some(v) = vol {
                if v.size_ok && !v.bad_pieces.contains(&s) {
                    available += 1;
                }
            }
        }
        if available < n {
            short += 1;
        }
    }
    short
}

/// Hash-check every piece of one volume; returns the stripe positions that do not match.
pub fn verify_volume(set: &VolumeSet, path: &Path, index: usize) -> Result<Vec<u32>> {
    let ps = set.piece_size as usize;
    let mut f = BufReader::with_capacity(ps.min(1 << 20), File::open(path)?);
    f.seek(SeekFrom::Start(HEADER_LEN as u64))?;
    let mut buf = vec![0u8; ps];
    let mut bad = Vec::new();
    let count = set.volume_pieces(index);
    for s in 0..count {
        let got = read_full(&mut f, &mut buf)?;
        let expected = if set.is_parity(index) { set.parity_hash(s as u32, index - set.data as usize) } else { set.piece_hash((s * set.data as u64 + index as u64) as u32) };
        if got != ps || chunk::hash(&buf) != expected {
            bad.push(s as u32);
        }
    }
    Ok(bad)
}

// ---------------------------------------------------------------- join / repair

/// Open the usable volumes of a set for random access.
fn open_volumes(status: &SetStatus) -> Result<Vec<Option<File>>> {
    let mut files = Vec::with_capacity(status.set.total());
    for v in &status.volumes {
        files.push(match v {
            Some(fv) if fv.size_ok => Some(File::open(&fv.path)?),
            _ => None,
        });
    }
    Ok(files)
}

/// Read stripe `s`: `data + parity` shards, `Some` only when present and hash-verified
/// (virtual pieces beyond the archive end are `Some(zeros)`).
fn read_stripe(set: &VolumeSet, files: &mut [Option<File>], s: u32, buf: &mut [Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
    let (n, ps) = (set.data as usize, set.piece_size as usize);
    let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(set.total());
    for vi in 0..set.total() {
        let virtual_piece = !set.is_parity(vi) && (s as u64 * n as u64 + vi as u64) >= set.pieces as u64;
        if virtual_piece {
            shards.push(Some(vec![0u8; ps]));
            continue;
        }
        let expected = if set.is_parity(vi) { set.parity_hash(s, vi - n) } else { set.piece_hash((s as u64 * n as u64 + vi as u64) as u32) };
        let shard = match &mut files[vi] {
            Some(f) => {
                let off = HEADER_LEN as u64 + s as u64 * ps as u64;
                let b = &mut buf[vi];
                let ok = f.seek(SeekFrom::Start(off)).is_ok() && read_full(f, b).map(|g| g == ps).unwrap_or(false) && chunk::hash(b) == expected;
                if ok {
                    Some(b.clone())
                } else {
                    None
                }
            }
            None => None,
        };
        shards.push(shard);
    }
    Ok(shards)
}

/// Rebuild the missing shards of a stripe in place (only the data shards when `data_only`);
/// error when too few are available. Returns the number of pieces rebuilt.
fn rebuild_stripe(set: &VolumeSet, rs: &Option<ReedSolomon>, s: u32, shards: &mut [Option<Vec<u8>>], data_only: bool) -> Result<u32> {
    let n = set.data as usize;
    let available = shards.iter().filter(|x| x.is_some()).count();
    if available < n {
        let missing: Vec<String> = shards.iter().enumerate().filter(|(_, x)| x.is_none()).map(|(i, _)| set.volume_name(i)).collect();
        return Err(Error::Missing(format!("stripe {s}: only {available} of the {n} pieces needed are intact; unavailable: {}", missing.join(", "))));
    }
    let wanted = if data_only { &shards[..n] } else { &shards[..] };
    let rebuilt = wanted.iter().filter(|x| x.is_none()).count() as u32;
    if rebuilt > 0 {
        let rs = rs.as_ref().ok_or_else(|| Error::Missing(format!("stripe {s}: a piece is missing and the set has no parity")))?;
        let res = if data_only { rs.reconstruct_data(shards) } else { rs.reconstruct(shards) };
        res.map_err(|e| Error::Corrupt(format!("reed-solomon reconstruct: {e:?}")))?;
    }
    Ok(rebuilt)
}

#[derive(Clone, Debug, Serialize)]
pub struct JoinReport {
    pub set_id: String,
    pub archive_name: String,
    pub archive_size: u64,
    pub output: PathBuf,
    pub volumes_used: Vec<usize>,
    pub volumes_missing: Vec<usize>,
    pub pieces_rebuilt: u32,
    pub archive_b3: String,
    pub archive_hash_ok: bool,
}

/// Rebuild the archive from the given volumes (files or directories) into `out`.
/// The result is written to a temporary name and renamed only after its hash matches the
/// descriptor; nothing is left behind on failure.
pub fn join(paths: &[PathBuf], out: &Path) -> Result<JoinReport> {
    let mut sets = inspect(paths, false)?;
    if sets.len() > 1 {
        return Err(Error::Invalid(format!("the inputs hold {} different volume sets: give the volumes of one set", sets.len())));
    }
    let status = sets.remove(0);
    let set = status.set.clone();
    let mut files = open_volumes(&status)?;
    let rs = rs_new(set.data as usize, set.parity as usize)?;
    let ps = set.piece_size as usize;
    let mut buf: Vec<Vec<u8>> = (0..set.total()).map(|_| vec![0u8; ps]).collect();
    let tmp = out.with_extension(format!("{}.tsr-partial", out.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_default()));
    let result = (|| -> Result<u32> {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        let mut hasher = blake3::Hasher::new();
        let mut written = 0u64;
        let mut rebuilt = 0u32;
        for s in 0..set.stripes {
            let mut shards = read_stripe(&set, &mut files, s, &mut buf)?;
            rebuilt += rebuild_stripe(&set, &rs, s, &mut shards, true)?;
            for (v, shard) in shards.iter().take(set.data as usize).enumerate() {
                let pi = s as u64 * set.data as u64 + v as u64;
                if pi >= set.pieces as u64 {
                    break;
                }
                let piece = shard.as_ref().ok_or_else(|| Error::Corrupt("reed-solomon left a shard empty".into()))?;
                let take = (set.archive_size - written).min(ps as u64) as usize;
                w.write_all(&piece[..take])?;
                hasher.update(&piece[..take]);
                written += take as u64;
            }
        }
        w.flush()?;
        if written != set.archive_size || hasher.finalize().as_bytes()[..] != set.archive_b3[..] {
            return Err(Error::HashMismatch("rebuilt archive does not match the hash recorded at split time".into()));
        }
        Ok(rebuilt)
    })();
    match result {
        Ok(rebuilt) => {
            if out.exists() {
                std::fs::remove_file(out)?;
            }
            std::fs::rename(&tmp, out)?;
            Ok(JoinReport {
                set_id: status.set_id.clone(),
                archive_name: set.archive_name.clone(),
                archive_size: set.archive_size,
                output: out.to_path_buf(),
                volumes_used: (0..set.total()).filter(|&i| files[i].is_some()).collect(),
                volumes_missing: status.missing.clone(),
                pieces_rebuilt: rebuilt,
                archive_b3: hex::encode(&set.archive_b3),
                archive_hash_ok: true,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RepairReport {
    pub set_id: String,
    pub rebuilt: Vec<VolumeInfo>,
    pub pieces_rebuilt: u32,
}

/// Recreate every missing or damaged volume of the set from the intact ones. New files go to
/// `out_dir` when given, otherwise next to the first intact volume; damaged files are replaced
/// in place. Recreated volumes are byte-identical to the originals.
pub fn repair(paths: &[PathBuf], out_dir: Option<&Path>) -> Result<RepairReport> {
    let mut sets = inspect(paths, true)?;
    if sets.len() > 1 {
        return Err(Error::Invalid(format!("the inputs hold {} different volume sets: give the volumes of one set", sets.len())));
    }
    let status = sets.remove(0);
    let set = status.set.clone();
    let targets: Vec<(usize, PathBuf)> = (0..set.total())
        .filter_map(|vi| match &status.volumes[vi] {
            Some(fv) if fv.usable() && fv.header_ok && fv.descriptor_ok => None,
            Some(fv) => Some((vi, fv.path.clone())),
            None => {
                let dir = out_dir.map(Path::to_path_buf).or_else(|| status.volumes.iter().flatten().next().map(|f| f.path.parent().map(Path::to_path_buf).unwrap_or_default()));
                dir.map(|d| (vi, d.join(set.volume_name(vi))))
            }
        })
        .collect();
    if targets.is_empty() {
        return Ok(RepairReport { set_id: status.set_id.clone(), rebuilt: Vec::new(), pieces_rebuilt: 0 });
    }
    if status.stripes_short > 0 {
        return Err(Error::Missing(format!("{} stripe(s) cannot be rebuilt with the volumes at hand", status.stripes_short)));
    }
    let mut files = open_volumes(&status)?;
    // damaged volumes are read for their intact pieces but never trusted blindly (hashes decide)
    let rs = rs_new(set.data as usize, set.parity as usize)?;
    let ps = set.piece_size as usize;
    let mut buf: Vec<Vec<u8>> = (0..set.total()).map(|_| vec![0u8; ps]).collect();
    let desc = format::cbor_encode(&set)?;
    let desc_hash = chunk::hash(&desc);
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&set.set_id);
    let tmp_paths: Vec<PathBuf> = targets.iter().map(|(_, p)| p.with_extension(format!("{EXTENSION}.tsr-partial"))).collect();
    let result = (|| -> Result<u32> {
        let mut writers: Vec<BufWriter<File>> = Vec::with_capacity(targets.len());
        for ((vi, _), tmp) in targets.iter().zip(tmp_paths.iter()) {
            let mut w = BufWriter::with_capacity(1 << 20, File::create(tmp)?);
            let header = VolumeHeader { index: *vi as u16, data: set.data, parity: set.parity, piece_size: set.piece_size, archive_size: set.archive_size, set_id, payload_len: set.payload_len(*vi) };
            w.write_all(&encode_header(&header))?;
            writers.push(w);
        }
        let mut rebuilt = 0u32;
        for s in 0..set.stripes {
            let mut shards = read_stripe(&set, &mut files, s, &mut buf)?;
            rebuilt += rebuild_stripe(&set, &rs, s, &mut shards, false)?;
            for ((vi, _), w) in targets.iter().zip(writers.iter_mut()) {
                if s as u64 >= set.volume_pieces(*vi) {
                    continue;
                }
                let piece = shards[*vi].as_ref().ok_or_else(|| Error::Corrupt("reed-solomon left a shard empty".into()))?;
                w.write_all(piece)?;
            }
        }
        for ((vi, _), mut w) in targets.iter().zip(writers.into_iter()) {
            w.write_all(&desc)?;
            w.write_all(&encode_trailer(HEADER_LEN as u64 + set.payload_len(*vi), desc.len() as u64, &desc_hash, *vi as u16))?;
            w.flush()?;
        }
        Ok(rebuilt)
    })();
    match result {
        Ok(pieces_rebuilt) => {
            drop(files);
            let mut rebuilt = Vec::new();
            for ((vi, path), tmp) in targets.iter().zip(tmp_paths.iter()) {
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                std::fs::rename(tmp, path)?;
                let bytes = std::fs::metadata(path)?.len();
                rebuilt.push(VolumeInfo { index: *vi, kind: if set.is_parity(*vi) { "parity" } else { "data" }, path: path.clone(), bytes });
            }
            Ok(RepairReport { set_id: status.set_id.clone(), rebuilt, pieces_rebuilt })
        }
        Err(e) => {
            for tmp in &tmp_paths {
                let _ = std::fs::remove_file(tmp);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_piece_size_boundaries() {
        // tiny and empty archives: the floor
        assert_eq!(adaptive_piece_size(0, 4), MIN_PIECE_SIZE);
        assert_eq!(adaptive_piece_size(1, 1), MIN_PIECE_SIZE);
        // the benchmark archive with 4 data volumes: 16 pieces per volume would be ~15 KiB, floor wins
        assert_eq!(adaptive_piece_size(956_731, 4), 64 << 10);
        // exactly 16 pieces of 64 KiB per volume: stays at 64 KiB; one byte more: 128 KiB
        assert_eq!(adaptive_piece_size(4 * 16 * 65_536, 4), 64 << 10);
        assert_eq!(adaptive_piece_size(4 * 16 * 65_536 + 1, 4), 128 << 10);
        // powers of two only
        assert_eq!(adaptive_piece_size(4 * 16 * 200_000, 4), 256 << 10);
        // the 1 MiB ceiling for ordinary sizes (64 MiB and 10 GiB with 4 data volumes)
        assert_eq!(adaptive_piece_size(64 << 20, 4), 1 << 20);
        assert_eq!(adaptive_piece_size(10 << 30, 4), 1 << 20);
        // beyond a million pieces the size grows, never above the format maximum
        assert_eq!(adaptive_piece_size(1_500_000u64 << 20, 4), 2 << 20);
        assert_eq!(adaptive_piece_size(u64::MAX / 4, 4), MAX_PIECE_SIZE);
        // fewer data volumes need bigger pieces for the same archive
        assert!(adaptive_piece_size(64 << 20, 1) >= adaptive_piece_size(64 << 20, 8));
    }
}
