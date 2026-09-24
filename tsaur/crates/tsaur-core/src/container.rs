//! Container awareness for ZIP-based formats (DOCX/XLSX/PPTX/EPUB/JAR/ODT/plain ZIP).
//!
//! The container is parsed with a small, strict ZIP framing parser that keeps the *exact* bytes
//! of every local header, central-directory record, data descriptor and the end-of-central-directory
//! block. Member payloads are handled by kind:
//!
//! * `stored` — the payload is the member's plaintext;
//! * `deflate` — the raw deflate stream is inverted with **preflate** (Microsoft, Apache-2.0),
//!   which yields the plaintext plus a small "corrections" blob that recreates the original stream
//!   bit-exact whatever encoder produced it (zlib, zlib-ng, miniz, .NET, ...);
//! * `raw` — anything else (unknown methods, encrypted members, streams preflate cannot model):
//!   the compressed bytes themselves are kept.
//!
//! The plaintext/raw payloads are what T-saur chunks, deduplicates and compresses. Rebuilding is
//! deterministic and is verified against the original hash at pack time; if the framing has gaps,
//! ZIP64 records or anything else we do not model, the whole file is stored `raw` instead.

use crate::error::{Error, Result};
use crate::manifest::{StreamRecipe, StreamSegment, ZipMemberMeta, ZipRecipe};
use preflate_rs::PreflateConfig;
use serde_bytes::ByteBuf;

pub const ZIP_EXTENSIONS: &[&str] = &["zip", "docx", "xlsx", "pptx", "epub", "jar", "odt", "ods", "odp", "xpi", "whl"];

pub const KIND_STORED: u8 = 0;
pub const KIND_DEFLATE: u8 = 1;
pub const KIND_RAW: u8 = 2;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;
const SIG_DESCRIPTOR: u32 = 0x0807_4b50;

/// Deflate streams above this plaintext size are kept raw (bounded memory, anti-bomb).
pub const PREFLATE_PLAINTEXT_LIMIT: usize = 256 << 20;

pub fn is_zip_like(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    ZIP_EXTENSIONS.contains(&ext.as_str())
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

struct CentralRecord {
    bytes_start: usize,
    bytes_end: usize,
    flags: u16,
    method: u16,
    csize: u32,
    usize_: u32,
    local_off: u32,
    name: String,
}

/// Parse the ZIP framing and invert the member payloads. Returns None for anything we do not
/// model exactly (the caller then stores the file raw).
pub fn explode(data: &[u8]) -> Option<(ZipRecipe, Vec<Vec<u8>>)> {
    if data.len() < 22 {
        return None;
    }
    // 1. End of central directory (search backwards, max comment 65535).
    let min_start = data.len().saturating_sub(22 + 65_535);
    let mut eocd_off = None;
    let mut i = data.len() - 22;
    loop {
        if u32_at(data, i) == SIG_EOCD {
            let clen = u16_at(data, i + 20) as usize;
            if i + 22 + clen == data.len() {
                eocd_off = Some(i);
                break;
            }
        }
        if i == min_start {
            break;
        }
        i -= 1;
    }
    let eocd_off = eocd_off?;
    if eocd_off >= 20 && u32_at(data, eocd_off - 20) == SIG_ZIP64_LOCATOR {
        return None; // ZIP64 is not modelled yet
    }
    let disk = u16_at(data, eocd_off + 4);
    let cd_disk = u16_at(data, eocd_off + 6);
    let entries = u16_at(data, eocd_off + 10) as usize;
    let cd_size = u32_at(data, eocd_off + 12) as usize;
    let cd_off = u32_at(data, eocd_off + 16) as usize;
    if disk != 0 || cd_disk != 0 || entries == 0xFFFF || cd_off + cd_size != eocd_off {
        return None;
    }

    // 2. Central directory records (exact bytes kept).
    let mut central = Vec::with_capacity(entries);
    let mut p = cd_off;
    for _ in 0..entries {
        if p + 46 > eocd_off || u32_at(data, p) != SIG_CENTRAL {
            return None;
        }
        let flags = u16_at(data, p + 8);
        let method = u16_at(data, p + 10);
        let csize = u32_at(data, p + 20);
        let usize_ = u32_at(data, p + 24);
        let nlen = u16_at(data, p + 28) as usize;
        let elen = u16_at(data, p + 30) as usize;
        let clen = u16_at(data, p + 32) as usize;
        let local_off = u32_at(data, p + 42);
        let end = p + 46 + nlen + elen + clen;
        if end > eocd_off || csize == u32::MAX || usize_ == u32::MAX || local_off == u32::MAX {
            return None;
        }
        let name = String::from_utf8_lossy(&data[p + 46..p + 46 + nlen]).to_string();
        central.push(CentralRecord { bytes_start: p, bytes_end: end, flags, method, csize, usize_, local_off, name });
        p = end;
    }
    if p != eocd_off {
        return None;
    }

    // 3. Local headers + payloads, in file order; the layout must be contiguous.
    let mut order: Vec<usize> = (0..central.len()).collect();
    order.sort_by_key(|&k| central[k].local_off);
    let mut members: Vec<Option<ZipMemberMeta>> = (0..central.len()).map(|_| None).collect();
    let mut parts: Vec<Option<Vec<u8>>> = (0..central.len()).map(|_| None).collect();
    let mut cursor = 0usize;
    let config = PreflateConfig { plain_text_limit: PREFLATE_PLAINTEXT_LIMIT, verify_compression: true, ..PreflateConfig::default() };
    for &k in &order {
        let c = &central[k];
        let lo = c.local_off as usize;
        if lo != cursor || lo + 30 > cd_off || u32_at(data, lo) != SIG_LOCAL {
            return None;
        }
        let nlen = u16_at(data, lo + 26) as usize;
        let elen = u16_at(data, lo + 28) as usize;
        let data_start = lo + 30 + nlen + elen;
        let data_end = data_start + c.csize as usize;
        if data_end > cd_off {
            return None;
        }
        let payload = &data[data_start..data_end];
        // optional data descriptor (general purpose bit 3)
        let mut desc_end = data_end;
        if c.flags & 0x0008 != 0 {
            if data_end + 16 <= cd_off && u32_at(data, data_end) == SIG_DESCRIPTOR {
                desc_end = data_end + 16;
            } else if data_end + 12 <= cd_off {
                desc_end = data_end + 12;
            } else {
                return None;
            }
        }
        let encrypted = c.flags & 0x0001 != 0;
        let (kind, plain, corrections) = if encrypted {
            (KIND_RAW, payload.to_vec(), Vec::new())
        } else {
            match c.method {
                0 => (KIND_STORED, payload.to_vec(), Vec::new()),
                8 => match preflate_rs::preflate_whole_deflate_stream(payload, &config) {
                    Ok((res, text)) if res.compressed_size == payload.len() && text.text().len() == c.usize_ as usize => (KIND_DEFLATE, text.text().to_vec(), res.corrections),
                    _ => (KIND_RAW, payload.to_vec(), Vec::new()),
                },
                _ => (KIND_RAW, payload.to_vec(), Vec::new()),
            }
        };
        members[k] = Some(ZipMemberMeta {
            name: c.name.clone(),
            kind,
            local_header: ByteBuf::from(data[lo..data_start].to_vec()),
            descriptor: ByteBuf::from(data[data_end..desc_end].to_vec()),
            central: ByteBuf::from(data[c.bytes_start..c.bytes_end].to_vec()),
            corrections: ByteBuf::from(corrections),
            size: plain.len() as u64,
            chunks: Vec::new(),
        });
        parts[k] = Some(plain);
        cursor = desc_end;
    }
    if cursor != cd_off {
        return None; // gap between the last member and the central directory
    }
    let members: Vec<ZipMemberMeta> = members.into_iter().map(|m| m.unwrap()).collect();
    let parts: Vec<Vec<u8>> = parts.into_iter().map(|p| p.unwrap()).collect();
    Some((ZipRecipe { local_order: order.iter().map(|&k| k as u32).collect(), eocd: ByteBuf::from(data[eocd_off..].to_vec()), members }, parts))
}

/// Rebuild the container from the recipe and the member payloads (same order as `recipe.members`).
pub fn rebuild(recipe: &ZipRecipe, parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    if recipe.members.len() != parts.len() || recipe.local_order.len() != parts.len() {
        return Err(Error::Corrupt("zip recipe/member count mismatch".into()));
    }
    let mut out = Vec::new();
    for &k in &recipe.local_order {
        let m = recipe.members.get(k as usize).ok_or_else(|| Error::Corrupt("zip local order out of range".into()))?;
        out.extend_from_slice(&m.local_header);
        match m.kind {
            KIND_STORED | KIND_RAW => out.extend_from_slice(&parts[k as usize]),
            KIND_DEFLATE => {
                let stream = preflate_rs::recreate_whole_deflate_stream(&parts[k as usize], &m.corrections).map_err(|e| Error::Corrupt(format!("preflate recreate {}: {e}", m.name)))?;
                out.extend_from_slice(&stream);
            }
            other => return Err(Error::Corrupt(format!("unknown zip member kind {other}"))),
        }
        out.extend_from_slice(&m.descriptor);
    }
    for m in &recipe.members {
        out.extend_from_slice(&m.central);
    }
    out.extend_from_slice(&recipe.eocd);
    Ok(out)
}

/// Explode and prove that the rebuild reproduces the exact input bytes.
pub fn try_bitexact(data: &[u8]) -> Option<(ZipRecipe, Vec<Vec<u8>>)> {
    let (recipe, parts) = explode(data)?;
    match rebuild(&recipe, &parts) {
        Ok(out) if out == data => Some((recipe, parts)),
        _ => None,
    }
}

// ----------------------------------------------------------------------------------------------
// Embedded zlib streams (PDF FlateDecode objects). Precomp-style: every `stream ... endstream`
// payload that starts with a valid zlib header is inverted with preflate; the Adler-32 trailer
// and the 2-byte header are kept so the rebuild is bit-exact. Everything else is literal.
// ----------------------------------------------------------------------------------------------

pub const STREAM_EXTENSIONS: &[&str] = &["pdf", "jpg", "jpeg", "jpe"];

pub fn is_stream_container(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    STREAM_EXTENSIONS.contains(&ext.as_str())
}

pub const SEG_LITERAL: u8 = 0;
pub const SEG_ZLIB: u8 = 1;
/// JPEG payload recompressed losslessly with Lepton (the chunked payload is the Lepton stream).
pub const SEG_JPEG: u8 = 2;

/// JPEG streams above this size are kept literal (bounded memory/time).
pub const LEPTON_MAX_INPUT: usize = 64 << 20;

#[cfg(feature = "lepton")]
fn lepton_features() -> lepton_jpeg::EnabledFeatures {
    lepton_jpeg::EnabledFeatures::compat_lepton_vector_write()
}

/// Lepton-encode a JPEG; the library verifies the round trip itself. Single-threaded so the
/// output is deterministic whatever the machine.
#[cfg(feature = "lepton")]
pub fn jpeg_to_lepton(jpeg: &[u8]) -> Option<Vec<u8>> {
    if jpeg.len() < 4 || jpeg.len() > LEPTON_MAX_INPUT || jpeg[..3] != [0xFF, 0xD8, 0xFF] {
        return None;
    }
    let pool = lepton_jpeg::SingleThreadPool {};
    match lepton_jpeg::catch_unwind_result(|| lepton_jpeg::encode_lepton_verify(jpeg, &lepton_features(), &pool)) {
        Ok((out, _metrics)) if out.len() < jpeg.len() => Some(out),
        _ => None,
    }
}

#[cfg(feature = "lepton")]
pub fn lepton_to_jpeg(lepton: &[u8], expected_len: u64) -> Result<Vec<u8>> {
    let pool = lepton_jpeg::SingleThreadPool {};
    let mut out = Vec::with_capacity(expected_len as usize);
    let mut reader = std::io::Cursor::new(lepton);
    lepton_jpeg::catch_unwind_result(|| lepton_jpeg::decode_lepton(&mut reader, &mut out, &lepton_features(), &pool)).map_err(|e| Error::Corrupt(format!("lepton decode: {e}")))?;
    Ok(out)
}

/// Without the `lepton` feature JPEGs are stored as they are (still bit-exact, just not smaller).
#[cfg(not(feature = "lepton"))]
pub fn jpeg_to_lepton(_jpeg: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(feature = "lepton"))]
pub fn lepton_to_jpeg(_lepton: &[u8], _expected_len: u64) -> Result<Vec<u8>> {
    Err(Error::Invalid("this build has no Lepton support (Cargo feature `lepton`): JPEG segments of this archive cannot be decoded".into()))
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn literal_segment(bytes: &[u8]) -> StreamSegment {
    StreamSegment { kind: SEG_LITERAL, header: ByteBuf::new(), trailer: ByteBuf::new(), corrections: ByteBuf::new(), size: bytes.len() as u64, chunks: Vec::new() }
}

/// Split a PDF (or similar) into literal segments, inverted zlib streams and, when `jpeg` is
/// set and the build has Lepton, recompressed JPEG streams. A file that is itself a JPEG becomes
/// a single JPEG segment (or, with `jpeg` off, no recipe at all: it is stored as it is, which
/// every build can read).
pub fn explode_streams(data: &[u8], jpeg: bool) -> Option<(StreamRecipe, Vec<Vec<u8>>)> {
    if data.len() >= 4 && data[..3] == [0xFF, 0xD8, 0xFF] {
        if !jpeg {
            return None;
        }
        let lepton = jpeg_to_lepton(data)?;
        let seg = StreamSegment {
            kind: SEG_JPEG,
            header: ByteBuf::new(),
            trailer: ByteBuf::from((data.len() as u64).to_le_bytes().to_vec()),
            corrections: ByteBuf::new(),
            size: lepton.len() as u64,
            chunks: Vec::new(),
        };
        return Some((StreamRecipe { segments: vec![seg] }, vec![lepton]));
    }
    let config = PreflateConfig { plain_text_limit: PREFLATE_PLAINTEXT_LIMIT, verify_compression: true, ..PreflateConfig::default() };
    let mut segments = Vec::new();
    let mut parts = Vec::new();
    let mut lit_start = 0usize;
    let mut pos = 0usize;
    let mut inverted = 0usize;
    while let Some(s) = find(data, b"stream", pos) {
        // must be the keyword `stream` followed by an end-of-line, not `endstream`
        let mut data_start = s + 6;
        if s >= 3 && &data[s - 3..s] == b"end" {
            pos = s + 6;
            continue;
        }
        if data.get(data_start) == Some(&b'\r') {
            data_start += 1;
        }
        if data.get(data_start) == Some(&b'\n') {
            data_start += 1;
        } else {
            pos = s + 6;
            continue;
        }
        let Some(end) = find(data, b"endstream", data_start) else { break };
        let payload = &data[data_start..end];
        // the payload usually ends with an EOL before `endstream`; the JPEG/zlib detectors compute the exact length
        let mut consumed: Option<(usize, StreamSegment, Vec<u8>)> = None;
        if payload.len() > 6 && payload[0] & 0x0F == 8 && ((payload[0] as u16) << 8 | payload[1] as u16).is_multiple_of(31) && payload[1] & 0x20 == 0 {
            if let Ok((res, text)) = preflate_rs::preflate_whole_deflate_stream(&payload[2..], &config) {
                let zlen = 2 + res.compressed_size + 4;
                if zlen <= payload.len() {
                    let trailer = &payload[2 + res.compressed_size..zlen];
                    if u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) == adler32(text.text()) {
                        let seg = StreamSegment {
                            kind: SEG_ZLIB,
                            header: ByteBuf::from(payload[..2].to_vec()),
                            trailer: ByteBuf::from(payload[zlen - 4..zlen].to_vec()),
                            corrections: ByteBuf::from(res.corrections),
                            size: text.text().len() as u64,
                            chunks: Vec::new(),
                        };
                        consumed = Some((zlen, seg, text.text().to_vec()));
                    }
                }
            }
        } else if jpeg && payload.len() > 4 && payload[..3] == [0xFF, 0xD8, 0xFF] {
            // JPEG (DCTDecode): trim trailing EOL bytes after the EOI marker, then Lepton-encode
            let mut jlen = payload.len();
            while jlen > 2 && (payload[jlen - 1] == b'\n' || payload[jlen - 1] == b'\r') {
                jlen -= 1;
            }
            if payload[jlen - 2..jlen] == [0xFF, 0xD9] {
                if let Some(lepton) = jpeg_to_lepton(&payload[..jlen]) {
                    let seg = StreamSegment {
                        kind: SEG_JPEG,
                        header: ByteBuf::new(),
                        trailer: ByteBuf::from((jlen as u64).to_le_bytes().to_vec()),
                        corrections: ByteBuf::new(),
                        size: lepton.len() as u64,
                        chunks: Vec::new(),
                    };
                    consumed = Some((jlen, seg, lepton));
                }
            }
        }
        if let Some((zlen, seg, plain)) = consumed {
            if data_start > lit_start {
                let lit = data[lit_start..data_start].to_vec();
                segments.push(literal_segment(&lit));
                parts.push(lit);
            }
            segments.push(seg);
            parts.push(plain);
            lit_start = data_start + zlen;
            inverted += 1;
        }
        pos = end + 9;
    }
    if inverted == 0 {
        return None;
    }
    if data.len() > lit_start {
        let lit = data[lit_start..].to_vec();
        segments.push(literal_segment(&lit));
        parts.push(lit);
    }
    Some((StreamRecipe { segments }, parts))
}

pub fn rebuild_streams(recipe: &StreamRecipe, parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    if recipe.segments.len() != parts.len() {
        return Err(Error::Corrupt("stream recipe/segment count mismatch".into()));
    }
    let mut out = Vec::new();
    for (seg, part) in recipe.segments.iter().zip(parts) {
        match seg.kind {
            SEG_LITERAL => out.extend_from_slice(part),
            SEG_ZLIB => {
                out.extend_from_slice(&seg.header);
                let stream = preflate_rs::recreate_whole_deflate_stream(part, &seg.corrections).map_err(|e| Error::Corrupt(format!("preflate recreate stream: {e}")))?;
                out.extend_from_slice(&stream);
                out.extend_from_slice(&seg.trailer);
            }
            SEG_JPEG => {
                let expected = if seg.trailer.len() == 8 { u64::from_le_bytes(seg.trailer[..8].try_into().unwrap()) } else { 0 };
                let jpeg = lepton_to_jpeg(part, expected)?;
                if expected != 0 && jpeg.len() as u64 != expected {
                    return Err(Error::Corrupt("lepton decode: unexpected JPEG length".into()));
                }
                out.extend_from_slice(&jpeg);
            }
            other => return Err(Error::Corrupt(format!("unknown stream segment kind {other}"))),
        }
    }
    Ok(out)
}

pub fn try_bitexact_streams(data: &[u8], jpeg: bool) -> Option<(StreamRecipe, Vec<Vec<u8>>)> {
    let (recipe, parts) = explode_streams(data, jpeg)?;
    match rebuild_streams(&recipe, &parts) {
        Ok(out) if out == data => Some((recipe, parts)),
        _ => None,
    }
}

/// Statistics helper for reports.
pub fn kinds(recipe: &ZipRecipe) -> (usize, usize, usize) {
    let mut s = (0, 0, 0);
    for m in &recipe.members {
        match m.kind {
            KIND_STORED => s.0 += 1,
            KIND_DEFLATE => s.1 += 1,
            _ => s.2 += 1,
        }
    }
    s
}
