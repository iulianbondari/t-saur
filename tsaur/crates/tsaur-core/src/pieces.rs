//! Transport pieces and recovery: the archive file is cut into fixed pieces; a `.pieces`
//! sidecar (like a `.torrent`) lists BLAKE3 hashes and a Merkle root; a `.par` sidecar holds
//! systematic Reed-Solomon parity (GF(2^8), stripes of at most 200 data pieces). Any `k` of the
//! `k + m` pieces of a stripe rebuild the stripe. Parity symbols are hashed too: FEC alone never
//! authenticates data.
//!
//! Everything streams: sidecar creation, verification and repair hold at most one stripe
//! (`MAX_STRIPE_BYTES` of data plus its parity) in memory, whatever the archive size.

use crate::chunk::{self, Hash};
use crate::error::{Error, Result};
use crate::format;
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAX_DATA_PER_STRIPE: usize = 200;
/// Upper bound on the data bytes of one stripe; caps `k` when pieces are large so that memory
/// stays bounded (parity adds at most `parity_ratio` on top).
pub const MAX_STRIPE_BYTES: usize = 64 << 20;
pub const MIN_PIECE_SIZE: u32 = 1024;
pub const MAX_PIECE_SIZE: u32 = 16 << 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StripeMeta {
    pub first: u32,
    pub k: u32,
    pub m: u32,
    /// Offset of this stripe's parity inside the `.par` file.
    pub par_off: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PiecesMeta {
    pub file: String,
    pub size: u64,
    pub piece_size: u32,
    pub pieces: Vec<ByteBuf>,
    pub parity: Vec<ByteBuf>,
    pub stripes: Vec<StripeMeta>,
    pub root: ByteBuf,
}

pub fn sidecar_paths(archive: &Path) -> (PathBuf, PathBuf) {
    let s = archive.to_string_lossy();
    (PathBuf::from(format!("{s}.pieces")), PathBuf::from(format!("{s}.par")))
}

fn stripe_k(piece_size: usize) -> usize {
    (MAX_STRIPE_BYTES / piece_size).clamp(2, MAX_DATA_PER_STRIPE)
}

fn check_piece_size(piece_size: u32) -> Result<usize> {
    if !(MIN_PIECE_SIZE..=MAX_PIECE_SIZE).contains(&piece_size) {
        return Err(Error::Invalid(format!("piece size must be between {MIN_PIECE_SIZE} and {MAX_PIECE_SIZE} bytes")));
    }
    Ok(piece_size as usize)
}

/// Fill `buf` from `r`; returns the number of bytes read (short at end of file, rest untouched).
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

/// Read one zero-padded piece at an absolute offset.
fn read_at(f: &mut File, off: u64, buf: &mut [u8]) -> Result<usize> {
    f.seek(SeekFrom::Start(off))?;
    let got = read_full(f, buf)?;
    buf[got..].fill(0);
    Ok(got)
}

fn rs_new(k: usize, m: usize) -> Result<ReedSolomon> {
    ReedSolomon::new(k, m).map_err(|e| Error::Invalid(format!("reed-solomon: {e:?}")))
}

pub fn write_sidecars(archive: &Path, piece_size: u32, parity_ratio: f64) -> Result<PiecesMeta> {
    let ps = check_piece_size(piece_size)?;
    if !(0.0..=1.0).contains(&parity_ratio) {
        return Err(Error::Invalid("parity ratio must be between 0 and 1".into()));
    }
    let mut input = BufReader::with_capacity(ps.min(1 << 20), File::open(archive)?);
    let size = input.get_ref().metadata()?.len();
    let n_pieces = usize::try_from(size.div_ceil(ps as u64)).map_err(|_| Error::Limit("too many pieces".into()))?;
    if n_pieces > u32::MAX as usize {
        return Err(Error::Limit("too many pieces".into()));
    }
    let k_max = stripe_k(ps);
    let (pieces_path, par_path) = sidecar_paths(archive);
    let mut par = BufWriter::new(File::create(&par_path)?);
    let mut hashes: Vec<Hash> = Vec::with_capacity(n_pieces);
    let mut parity_hashes: Vec<Hash> = Vec::new();
    let mut stripes = Vec::new();
    let mut par_off = 0u64;
    let mut first = 0usize;
    while first < n_pieces {
        let k = (n_pieces - first).min(k_max);
        let m = ((k as f64 * parity_ratio).round() as usize).max(1);
        let mut shards: Vec<Vec<u8>> = Vec::with_capacity(k + m);
        for _ in 0..k {
            let mut buf = vec![0u8; ps];
            read_full(&mut input, &mut buf)?;
            hashes.push(chunk::hash(&buf));
            shards.push(buf);
        }
        shards.extend((0..m).map(|_| vec![0u8; ps]));
        rs_new(k, m)?.encode(&mut shards).map_err(|e| Error::Invalid(format!("reed-solomon encode: {e:?}")))?;
        for p in &shards[k..] {
            parity_hashes.push(chunk::hash(p));
            par.write_all(p)?;
        }
        stripes.push(StripeMeta { first: first as u32, k: k as u32, m: m as u32, par_off });
        par_off += (m * ps) as u64;
        first += k;
    }
    par.flush()?;
    let meta = PiecesMeta {
        file: archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        size,
        piece_size,
        pieces: hashes.iter().map(|h| ByteBuf::from(h.to_vec())).collect(),
        parity: parity_hashes.iter().map(|h| ByteBuf::from(h.to_vec())).collect(),
        stripes,
        root: ByteBuf::from(chunk::merkle_root(&hashes).to_vec()),
    };
    std::fs::write(pieces_path, format::cbor_encode(&meta)?)?;
    Ok(meta)
}

pub fn load_meta(archive: &Path) -> Result<PiecesMeta> {
    let (pieces_path, _) = sidecar_paths(archive);
    let bytes = std::fs::read(&pieces_path).map_err(|_| Error::Missing(format!("{} not found", pieces_path.display())))?;
    let meta: PiecesMeta = format::cbor_decode(&bytes)?;
    check_piece_size(meta.piece_size).map_err(|_| Error::Corrupt("pieces sidecar: piece size out of range".into()))?;
    let n = meta.pieces.len();
    let expected = meta.size.div_ceil(meta.piece_size as u64);
    if n as u64 != expected {
        return Err(Error::Corrupt("pieces sidecar: piece count does not match the size".into()));
    }
    let mut par_count = 0usize;
    let mut next = 0u64;
    for st in &meta.stripes {
        if st.first as u64 != next || st.k == 0 || st.first as u64 + st.k as u64 > n as u64 || st.k as usize + st.m as usize > 256 {
            return Err(Error::Corrupt("pieces sidecar: inconsistent stripe table".into()));
        }
        next += st.k as u64;
        par_count += st.m as usize;
    }
    if next != n as u64 || par_count != meta.parity.len() || meta.pieces.iter().chain(meta.parity.iter()).any(|h| h.len() != 32) {
        return Err(Error::Corrupt("pieces sidecar: inconsistent stripe table".into()));
    }
    Ok(meta)
}

/// Returns the indices of corrupt/missing pieces (streaming, one piece in memory at a time).
/// A file whose length differs from the recorded size has its last piece flagged so that
/// `recover` restores the exact length.
pub fn verify(archive: &Path) -> Result<(Vec<u32>, PiecesMeta)> {
    let meta = load_meta(archive)?;
    let ps = meta.piece_size as usize;
    let mut bad = Vec::new();
    match File::open(archive) {
        Ok(f) => {
            let actual = f.metadata()?.len();
            let mut r = BufReader::with_capacity(ps.min(1 << 20), f);
            let mut buf = vec![0u8; ps];
            for (i, h) in meta.pieces.iter().enumerate() {
                let got = read_full(&mut r, &mut buf)?;
                buf[got..].fill(0);
                if chunk::hash(&buf)[..] != h[..] {
                    bad.push(i as u32);
                }
            }
            if actual != meta.size && !meta.pieces.is_empty() {
                let last = meta.pieces.len() as u32 - 1;
                if !bad.contains(&last) {
                    bad.push(last);
                }
            }
        }
        Err(_) => bad = (0..meta.pieces.len() as u32).collect(),
    }
    Ok((bad, meta))
}

/// Rebuild corrupt pieces from parity, stripe by stripe, writing repaired pieces in place.
/// Returns (pieces repaired, archive verifies afterwards). Stripes with more damage than parity
/// are skipped; every other stripe is still repaired.
pub fn recover(archive: &Path) -> Result<(u32, bool)> {
    let (bad, meta) = verify(archive)?;
    if bad.is_empty() {
        return Ok((0, true));
    }
    let ps = meta.piece_size as usize;
    let (_, par_path) = sidecar_paths(archive);
    let mut par = File::open(&par_path).map_err(|_| Error::Missing(format!("{} not found", par_path.display())))?;
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(archive)?;
    if file.metadata()?.len() != meta.size {
        file.set_len(meta.size)?;
    }
    let mut repaired = 0u32;
    let mut par_index = 0usize;
    let mut all_ok = true;
    for st in &meta.stripes {
        let (k, m, first) = (st.k as usize, st.m as usize, st.first as usize);
        let stripe_bad: Vec<usize> = bad.iter().map(|&b| b as usize).filter(|&b| b >= first && b < first + k).collect();
        if stripe_bad.is_empty() {
            par_index += m;
            continue;
        }
        let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(k + m);
        for i in first..first + k {
            if stripe_bad.contains(&i) {
                shards.push(None);
            } else {
                let mut buf = vec![0u8; ps];
                read_at(&mut file, (i * ps) as u64, &mut buf)?;
                shards.push(Some(buf));
            }
        }
        for j in 0..m {
            let mut buf = vec![0u8; ps];
            let got = read_at(&mut par, st.par_off + (j * ps) as u64, &mut buf)?;
            let ok = got == ps && meta.parity.get(par_index + j).is_some_and(|h| h[..] == chunk::hash(&buf)[..]);
            shards.push(if ok { Some(buf) } else { None });
        }
        par_index += m;
        let available = shards.iter().filter(|s| s.is_some()).count();
        if available < k {
            all_ok = false;
            continue;
        }
        rs_new(k, m)?.reconstruct(&mut shards).map_err(|e| Error::Corrupt(format!("reed-solomon reconstruct: {e:?}")))?;
        for &i in &stripe_bad {
            let piece = shards[i - first].as_ref().ok_or_else(|| Error::Corrupt("reed-solomon left a shard empty".into()))?;
            let start = (i * ps) as u64;
            let end = (start + ps as u64).min(meta.size);
            file.seek(SeekFrom::Start(start))?;
            file.write_all(&piece[..(end - start) as usize])?;
            repaired += 1;
        }
    }
    file.flush()?;
    drop(file);
    let (bad_after, _) = verify(archive)?;
    Ok((repaired, all_ok && bad_after.is_empty()))
}
