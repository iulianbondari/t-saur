//! Compression codecs. v0.1 ships store, zstd, zstd-with-dictionary and xz (LZMA2); the codec id
//! space is reserved for context mixing, model-predictive, delta and float-split codecs (spec §4.2).

use crate::error::{Error, Result};
use std::io::{Read, Write};
use zstd::stream::raw::{CParameter, DParameter};

pub const STORE: u8 = 0;
pub const ZSTD: u8 = 1;
pub const ZSTD_DICT: u8 = 2;
pub const XZ: u8 = 3;
pub const PPMD: u8 = 4;
/// zstd with a raw-content dictionary made of chunks of an earlier version (`BlobRecord.d`).
pub const ZSTD_DELTA: u8 = 5;

pub fn name(codec: u8) -> &'static str {
    match codec {
        STORE => "store",
        ZSTD => "zstd",
        ZSTD_DICT => "zstd+dict",
        XZ => "xz",
        PPMD => "ppmd",
        ZSTD_DELTA => "zstd+delta",
        _ => "unknown",
    }
}

/// Which codecs the packer may try for a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecChoice {
    /// zstd only (fast)
    Zstd,
    /// xz/LZMA2 only (slower, usually smaller on text)
    Xz,
    /// PPMd (variant H) only — text specialist, symmetric (decoding as slow as encoding)
    Ppmd,
    /// try zstd and xz (and PPMd on text-like blocks) and keep the smallest output (default)
    Best,
}

impl CodecChoice {
    pub fn by_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "zstd" => Some(Self::Zstd),
            "xz" | "lzma" | "lzma2" => Some(Self::Xz),
            "ppmd" => Some(Self::Ppmd),
            "best" | "auto" => Some(Self::Best),
            _ => None,
        }
    }
}

/// Encoded output plus the parameters the decoder needs.
#[derive(Clone, Debug)]
pub struct Encoded {
    pub codec: u8,
    pub bytes: Vec<u8>,
    pub params: Vec<u32>,
    /// Pre-filter applied before the codec (0 = none, 1 = x86 BCJ, 2 = ARM64 BCJ).
    pub filter: u8,
}

pub const FILTER_NONE: u8 = 0;
pub const FILTER_X86: u8 = 1;

/// x86 BCJ branch converter (the classic LZMA SDK `x86.c` transform, public domain): rewrites the
/// relative targets of `E8`/`E9` CALL/JMP instructions into absolute addresses so that repeated
/// calls to the same function become identical byte sequences. Symmetric: `encoding = false`
/// restores the original bytes exactly. Returns the number of bytes fully processed.
pub fn x86_convert(data: &mut [u8], ip: u32, state: &mut u32, encoding: bool) -> usize {
    #[inline]
    fn test_ms_byte(b: u8) -> bool {
        (b.wrapping_add(1) & 0xFE) == 0
    }
    if data.len() < 5 {
        return 0;
    }
    let size = data.len() - 4;
    let ip = ip.wrapping_add(5);
    let mut pos = 0usize;
    let mut mask = *state & 7;
    loop {
        let mut p = pos;
        while p < size && (data[p] & 0xFE) != 0xE8 {
            p += 1;
        }
        let d = p - pos;
        pos = p;
        if p >= size {
            *state = if d > 2 { 0 } else { mask >> d };
            return pos;
        }
        if d > 2 {
            mask = 0;
        } else {
            mask >>= d;
            if mask != 0 && (mask > 4 || mask == 3 || test_ms_byte(data[p + (mask as usize >> 1) + 1])) {
                mask = (mask >> 1) | 4;
                pos += 1;
                continue;
            }
        }
        if test_ms_byte(data[p + 4]) {
            let mut v = u32::from_le_bytes([data[p + 1], data[p + 2], data[p + 3], data[p + 4]]);
            let cur = ip.wrapping_add(pos as u32);
            pos += 5;
            if encoding {
                v = v.wrapping_add(cur);
            } else {
                v = v.wrapping_sub(cur);
            }
            if mask != 0 {
                let sh = (mask & 6) << 2;
                if test_ms_byte((v >> sh) as u8) {
                    v ^= (0x100u32 << sh).wrapping_sub(1);
                    if encoding {
                        v = v.wrapping_add(cur);
                    } else {
                        v = v.wrapping_sub(cur);
                    }
                }
                mask = 0;
            }
            data[p + 1] = v as u8;
            data[p + 2] = (v >> 8) as u8;
            data[p + 3] = (v >> 16) as u8;
            data[p + 4] = 0u8.wrapping_sub(((v >> 24) & 1) as u8);
        } else {
            mask = (mask >> 1) | 4;
            pos += 1;
        }
    }
}

/// Heuristic: x86/x64 machine code has many `E8`/`E9` CALL/JMP opcodes whose rel32 operand has a
/// most-significant byte of `00` or `FF` (near targets). Random bytes hit that pattern about
/// 0.006 % of the time; real code sections hit it 0.5–2 % of the time.
pub fn looks_like_x86(data: &[u8]) -> bool {
    if data.len() < 4096 || looks_like_text(data) {
        return false;
    }
    let sample = &data[..data.len().min(256 << 10)];
    let hits = sample.windows(5).filter(|w| (w[0] & 0xFE) == 0xE8 && (w[4] == 0 || w[4] == 0xFF)).count();
    hits * 1000 / sample.len() >= 1
}

/// ARM64 branch converter (the `arm64` BCJ filter from XZ Utils, 0BSD): converts the immediate of
/// `BL` and `ADRP` instructions from PC-relative to absolute form. Symmetric, works on 4-byte words.
pub fn arm64_convert(data: &mut [u8], now_pos: u32, encoding: bool) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let pc = now_pos.wrapping_add(i as u32);
        let mut instr = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        if (instr >> 26) == 0x25 {
            let src = instr;
            instr = 0x9400_0000;
            let mut p = pc >> 2;
            if !encoding {
                p = 0u32.wrapping_sub(p);
            }
            instr |= src.wrapping_add(p) & 0x03FF_FFFF;
            data[i..i + 4].copy_from_slice(&instr.to_le_bytes());
        } else if (instr & 0x9F00_0000) == 0x9000_0000 {
            let src = ((instr >> 29) & 3) | ((instr >> 3) & 0x001F_FFFC);
            if (src.wrapping_add(0x0002_0000) & 0x001C_0000) == 0 {
                instr &= 0x9000_001F;
                let mut p = pc >> 12;
                if !encoding {
                    p = 0u32.wrapping_sub(p);
                }
                let dest = src.wrapping_add(p);
                instr |= (dest & 3) << 29;
                instr |= (dest & 0x0003_FFFC) << 3;
                instr |= 0u32.wrapping_sub(dest & 0x0002_0000) & 0x00E0_0000;
                data[i..i + 4].copy_from_slice(&instr.to_le_bytes());
            }
        }
        i += 4;
    }
    i
}

/// Heuristic: ARM64 code is a stream of 4-byte instructions with frequent `BL` calls to near
/// targets (imm26 sign-extended, magnitude below 2^20 words). Random words qualify 0.05 % of the time;
/// real code 1–5 %. Blocks start at arbitrary chunk boundaries, so all four word alignments are
/// tried; returns the byte offset of the first aligned instruction word.
pub fn arm64_alignment(data: &[u8]) -> Option<u8> {
    if data.len() < 4096 || looks_like_text(data) {
        return None;
    }
    let sample = &data[..data.len().min(256 << 10)];
    let words = sample.len() / 4;
    let mut best = (0usize, 0u8);
    for off in 0..4usize {
        let hits = sample[off..]
            .chunks_exact(4)
            .filter(|w| {
                let v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                (v >> 26) == 0x25 && matches!((v >> 20) & 0x3F, 0 | 0x3F)
            })
            .count();
        if hits > best.0 {
            best = (hits, off as u8);
        }
    }
    (best.0 * 1000 / words.max(1) >= 5).then_some(best.1)
}

/// Filter byte layout: low nibble = filter id, high nibble = filter parameter
/// (ARM64: byte offset of the first aligned instruction word, 0..3).
pub fn filter_id(f: u8) -> u8 {
    f & 0x0F
}

pub fn filter_param(f: u8) -> u8 {
    f >> 4
}

pub const FILTER_ARM64: u8 = 2;

fn apply_filter(f: u8, data: &[u8], encoding: bool) -> Result<Vec<u8>> {
    let mut v = data.to_vec();
    match filter_id(f) {
        FILTER_NONE => {}
        FILTER_X86 => {
            let mut state = 0u32;
            x86_convert(&mut v, 0, &mut state, encoding);
        }
        FILTER_ARM64 => {
            let off = (filter_param(f) as usize).min(3).min(v.len());
            arm64_convert(&mut v[off..], 0, encoding);
        }
        other => return Err(Error::Corrupt(format!("unknown filter id {other}"))),
    }
    Ok(v)
}

/// PPMd model order / memory chosen deterministically from the block size: memory grows with the
/// block (16 MiB..256 MiB), order 16 is a good text default for PPMd H.
pub fn ppmd_params(len: usize) -> (u32, u32) {
    let mem = ((len as u64) * 16).clamp(16 << 20, 256 << 20) as u32;
    (16, mem)
}

fn ppmd_compress(data: &[u8], order: u32, mem: u32) -> Result<Vec<u8>> {
    let mut enc = ppmd_rust::Ppmd7Encoder::new(Vec::with_capacity(data.len() / 3 + 64), order, mem).map_err(|e| Error::Encoding(format!("ppmd: {e:?}")))?;
    enc.write_all(data)?;
    Ok(enc.finish(true)?)
}

fn ppmd_decompress(data: &[u8], ulen: usize, order: u32, mem: u32) -> Result<Vec<u8>> {
    let mut dec = ppmd_rust::Ppmd7Decoder::new(data, order, mem).map_err(|e| Error::Corrupt(format!("ppmd: {e:?}")))?;
    let mut out = vec![0u8; ulen];
    dec.read_exact(&mut out)?;
    Ok(out)
}

/// Heuristic: does the block look like text (worth a PPMd attempt)? Samples up to 64 KiB.
pub fn looks_like_text(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(64 << 10)];
    if sample.is_empty() {
        return false;
    }
    let textish = sample.iter().filter(|&&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7f).contains(&b) || b >= 0x80).count();
    let zeros = sample.iter().filter(|&&b| b == 0).count();
    zeros == 0 && textish * 100 / sample.len() >= 97
}

#[derive(Clone, Debug)]
pub struct CodecOptions {
    pub level: i32,
    /// Upper bound for the zstd window log; the window is otherwise sized to the block.
    pub long_window_log: u32,
    pub dict: Option<Vec<u8>>,
    pub choice: CodecChoice,
    /// Codec-choice effort for `CodecChoice::Best`, 1..=5 (values outside are clamped):
    /// 1 = zstd only (identical to `CodecChoice::Zstd`), 2 = zstd or xz decided on a sample of the
    /// block, 3 = zstd, xz or PPMd (text) decided on a sample, 4 = 3 plus xz on the whole block when
    /// PPMd won the sample, 5 = every codec on every block (the default; today's full trial, the
    /// same code path byte for byte). Blocks of at most [`sample_full_trial_max`] bytes always run
    /// the full trial. Other choices ignore it. See [`sample`] for the sampling rule.
    pub effort: u8,
}

impl Default for CodecOptions {
    fn default() -> Self {
        Self { level: 19, long_window_log: 27, dict: None, choice: CodecChoice::Zstd, effort: EFFORT_DEFAULT }
    }
}

/// Default `effort`: the full trial.
pub const EFFORT_DEFAULT: u8 = 5;

/// Length of one slice of the codec-choice sample (§2.1 of `docs/design/plans/1.1-codec-sampling.md`).
pub const SAMPLE_SLICE: usize = 8 << 10;

/// Number of slices the sample of a block of `len` bytes has: one per 128 KiB, at least 8 (a
/// 1 MiB block gives 8 slices = 64 KiB) and at most 32 (256 KiB at 4 MiB and above). A block prefix
/// must not be used instead: blocks concatenate chunks of several files and a prefix predicts the
/// wrong codec on a third of the benchmark blocks (measured, see the plan).
pub fn sample_slices(len: usize) -> usize {
    (len / (128 << 10)).clamp(8, 32)
}

/// Largest block that is never sampled: `2 * slices * SAMPLE_SLICE` (128 KiB for blocks up to
/// 1 MiB), below which a sampled trial costs about as much as the full one. Every golden archive
/// holds one blob below this size, so efforts 2..=5 reproduce them by construction.
pub fn sample_full_trial_max(len: usize) -> usize {
    2 * sample_slices(len) * SAMPLE_SLICE
}

/// Byte offsets of the sample slices of a block of `len` bytes, evenly spaced from 0 to
/// `len - SAMPLE_SLICE`, each rounded down to a multiple of 4 so that the word alignment
/// `arm64_alignment` finds on the sample is the block's. Empty when the block is not sampled.
pub fn sample_offsets(len: usize) -> Vec<usize> {
    if len <= sample_full_trial_max(len) {
        return Vec::new();
    }
    let n = sample_slices(len);
    let span = (len - SAMPLE_SLICE) as u64;
    (0..n).map(|i| ((i as u64 * span / (n as u64 - 1)) as usize) & !3).collect()
}

/// The codec-choice sample of a block: the block itself (borrowed) when it is small enough for
/// the full trial, otherwise the concatenation of its sample slices. A pure function of the bytes.
pub fn sample(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let offsets = sample_offsets(data.len());
    if offsets.is_empty() {
        return std::borrow::Cow::Borrowed(data);
    }
    let mut s = Vec::with_capacity(offsets.len() * SAMPLE_SLICE);
    for off in offsets {
        s.extend_from_slice(&data[off..off + SAMPLE_SLICE]);
    }
    std::borrow::Cow::Owned(s)
}

/// Does `compress_best` decide the codec of a block of `len` bytes on a sample with these options?
/// (`Best` at efforts 2..=4 and a block above `sample_full_trial_max`.)
pub fn samples_block(len: usize, o: &CodecOptions) -> bool {
    o.choice == CodecChoice::Best && (2..=4).contains(&o.effort) && len > sample_full_trial_max(len)
}

fn zstd_compress(data: &[u8], level: i32, max_window_log: u32) -> Result<Vec<u8>> {
    let mut c = zstd::bulk::Compressor::new(level)?;
    // The window never needs to exceed the block: this caps the memory of every parallel
    // compressor (level 19 with a 128 MiB window costs hundreds of MB per context) without
    // changing the output for blocks that fit in the window. Long-distance matching across
    // blocks is unnecessary because deduplication already happens at chunk level.
    let needed = usize::BITS - data.len().max(1024).leading_zeros(); // ceil(log2(len))
    let window_log = needed.clamp(10, if max_window_log > 0 { max_window_log } else { 23 });
    c.set_parameter(CParameter::WindowLog(window_log))?;
    Ok(c.compress(data)?)
}

/// xz/LZMA2 preset derived from the zstd-style level: 1..=9 map to xz presets 1..=9, higher levels
/// use preset 9 extreme. The LZMA2 dictionary is sized to the block (never larger than needed), which
/// keeps the memory of parallel encoders proportional to the block size instead of the preset's 64 MiB.
fn xz_compress(data: &[u8], level: i32) -> Result<Vec<u8>> {
    use liblzma::stream::{Check, Filters, LzmaOptions, Stream};
    let preset: u32 = if level >= 15 { 9 | (1u32 << 31) } else { (level.clamp(1, 9)) as u32 };
    let mut opts = LzmaOptions::new_preset(preset).map_err(|e| Error::Encoding(format!("xz preset: {e}")))?;
    let dict = (data.len().max(4096) as u32).next_power_of_two().min(64 << 20);
    opts.dict_size(dict);
    let mut filters = Filters::new();
    filters.lzma2(&opts);
    let stream = Stream::new_stream_encoder(&filters, Check::None).map_err(|e| Error::Encoding(format!("xz encoder: {e}")))?;
    let mut enc = liblzma::write::XzEncoder::new_stream(Vec::with_capacity(data.len() / 2 + 64), stream);
    enc.write_all(data)?;
    Ok(enc.finish()?)
}

/// zstd with `prefix` as a raw-content dictionary (the "patch-from" model): matches may reach into
/// the prefix, so a new version of a document costs about the size of its edits.
pub fn zstd_compress_with_prefix(data: &[u8], level: i32, prefix: &[u8]) -> Result<Vec<u8>> {
    let need = (prefix.len() + data.len()).max(1024);
    let window_log = (usize::BITS - (need - 1).leading_zeros()).clamp(10, 27);
    let mut c = zstd::bulk::Compressor::new(level)?;
    c.set_parameter(CParameter::WindowLog(window_log))?;
    c.set_dictionary(level, prefix)?;
    Ok(c.compress(data)?)
}

pub fn zstd_decompress_with_prefix(data: &[u8], ulen: usize, prefix: &[u8]) -> Result<Vec<u8>> {
    let mut d = zstd::bulk::Decompressor::with_dictionary(prefix)?;
    d.set_parameter(DParameter::WindowLogMax(31))?;
    let out = d.decompress(data, ulen)?;
    if out.len() != ulen {
        return Err(Error::Corrupt(format!("decoded {} bytes, expected {ulen}", out.len())));
    }
    Ok(out)
}

/// The filter candidates of a block: none, plus the x86 or the ARM64 branch converter when the
/// bytes look like machine code (`data` is the whole block, or its sample at efforts 2..=4).
fn filter_candidates(data: &[u8]) -> Vec<u8> {
    let mut filters = vec![FILTER_NONE];
    if looks_like_x86(data) {
        filters.push(FILTER_X86);
    } else if let Some(off) = arm64_alignment(data) {
        filters.push(FILTER_ARM64 | (off << 4));
    }
    filters
}

/// The codecs one filter variant of `compress_trial` runs.
#[derive(Clone, Copy, Debug)]
enum CodecSet<'a> {
    /// The rule of `CodecOptions::choice`: zstd (and zstd+dict when a dictionary is given), xz,
    /// PPMd on input that looks like text, and for `Best` the incompressible shortcut (zstd cannot
    /// shrink the input by 3 %: the slower codecs are skipped).
    ByChoice,
    /// Exactly these codec ids (decided on a sample); no shortcut.
    Only(&'a [u8]),
}

/// Run the given filter variants on a block and keep the smallest output; store if nothing is
/// smaller than the block. The tie-break is *first tried wins*: variants in the given order, and
/// within a variant zstd, zstd+dict, xz, PPMd (an output replaces the best only when strictly smaller).
fn compress_trial(data: &[u8], o: &CodecOptions, variants: &[(u8, CodecSet)]) -> Result<Encoded> {
    let mut best = Encoded { codec: STORE, bytes: data.to_vec(), params: Vec::new(), filter: FILTER_NONE };
    for &(filter, set) in variants {
        let filtered;
        let input: &[u8] = if filter == FILTER_NONE {
            data
        } else {
            filtered = apply_filter(filter, data, true)?;
            &filtered
        };
        let mut consider = |codec: u8, out: Vec<u8>, params: Vec<u32>| {
            if out.len() < best.bytes.len() {
                best = Encoded { codec, bytes: out, params, filter };
            }
        };
        let (try_zstd, try_dict, try_xz, try_ppmd) = match set {
            CodecSet::ByChoice => {
                let zstd = matches!(o.choice, CodecChoice::Zstd | CodecChoice::Best);
                (zstd, zstd, matches!(o.choice, CodecChoice::Xz | CodecChoice::Best), o.choice == CodecChoice::Ppmd || (o.choice == CodecChoice::Best && looks_like_text(input)))
            }
            CodecSet::Only(codecs) => (codecs.contains(&ZSTD), codecs.contains(&ZSTD_DICT), codecs.contains(&XZ), codecs.contains(&PPMD)),
        };
        let mut incompressible = false;
        if try_zstd {
            let z = zstd_compress(input, o.level, o.long_window_log)?;
            incompressible = z.len() * 100 >= input.len() * 97;
            consider(ZSTD, z, Vec::new());
        }
        if try_dict {
            if let Some(d) = &o.dict {
                let mut c = zstd::bulk::Compressor::with_dictionary(o.level, d)?;
                consider(ZSTD_DICT, c.compress(input)?, Vec::new());
            }
        }
        if incompressible && o.choice == CodecChoice::Best && matches!(set, CodecSet::ByChoice) {
            continue;
        }
        if try_xz && input.len() >= 256 {
            consider(XZ, xz_compress(input, o.level)?, Vec::new());
        }
        if try_ppmd && input.len() >= 1024 {
            let (order, mem) = ppmd_params(input.len());
            consider(PPMD, ppmd_compress(input, order, mem)?, vec![order, mem]);
        }
    }
    Ok(best)
}

/// What a sample decided for a block (`decide_on_sample`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Filter byte (id and parameter) to apply to the whole block.
    pub filter: u8,
    /// Codec to run on the whole block.
    pub codec: u8,
    /// A second codec to run on the whole block with the same filter, keeping the smaller output
    /// (effort 4: xz whenever PPMd won the sample, the one structural miss of a sample).
    pub also: Option<u8>,
    /// zstd could not shrink the unfiltered sample by 3 % (the shortcut of the full trial, taken
    /// on the sample: xz and PPMd were not tried for that variant).
    pub incompressible: bool,
}

/// One candidate of the sample decision: `(filter, codec, size of the sample's encoding)`.
type Candidate = (u8, u8, usize);

/// The candidate `consider()` would have kept: the first strictly smallest one in push order
/// (unfiltered before filtered; zstd, zstd+dict, xz, PPMd within a filter).
fn pick(candidates: &[Candidate]) -> Option<Candidate> {
    let mut best: Option<Candidate> = None;
    for &c in candidates {
        if best.is_none_or(|b| c.2 < b.2) {
            best = Some(c);
        }
    }
    best
}

/// Decide filter and codec for a block on its sample (`sample()` of the block; efforts 2..=4).
/// The same heuristics and codecs as the full trial run on the sample, in the same order, and
/// the candidate the full trial would have kept on the sample wins. `looks_like_text` is judged on
/// the (filtered) sample here, not on the first 64 KiB of the block as in the full trial: a strided
/// sample represents a mixed block better; this is intended.
pub fn decide_on_sample(sample: &[u8], o: &CodecOptions) -> Result<Decision> {
    let effort = o.effort.clamp(1, 5);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut incompressible_unfiltered = false;
    for filter in filter_candidates(sample) {
        let filtered;
        let input: &[u8] = if filter == FILTER_NONE {
            sample
        } else {
            filtered = apply_filter(filter, sample, true)?;
            &filtered
        };
        let z = zstd_compress(input, o.level, o.long_window_log)?;
        let incompressible = z.len() * 100 >= input.len() * 97;
        if filter == FILTER_NONE {
            incompressible_unfiltered = incompressible;
        }
        candidates.push((filter, ZSTD, z.len()));
        if let Some(d) = &o.dict {
            let mut c = zstd::bulk::Compressor::with_dictionary(o.level, d)?;
            candidates.push((filter, ZSTD_DICT, c.compress(input)?.len()));
        }
        if incompressible {
            continue;
        }
        if effort >= 2 && input.len() >= 256 {
            candidates.push((filter, XZ, xz_compress(input, o.level)?.len()));
        }
        if effort >= 3 && input.len() >= 1024 && looks_like_text(input) {
            let (order, mem) = ppmd_params(input.len());
            candidates.push((filter, PPMD, ppmd_compress(input, order, mem)?.len()));
        }
    }
    let (filter, codec, _) = pick(&candidates).expect("zstd is always a candidate");
    let also = (effort == 4 && codec == PPMD).then_some(XZ);
    Ok(Decision { filter, codec, also, incompressible: incompressible_unfiltered })
}

/// Try every applicable codec (with and without a branch filter when the block looks like x86 or
/// ARM64 machine code) and keep the smallest output; store if nothing is smaller. Blocks that zstd cannot shrink
/// by at least 3 % are treated as incompressible and the slower codecs are skipped.
///
/// `o.effort` (for `CodecChoice::Best` only) trades codec-choice time for ratio: at effort 5 (the
/// default) and for blocks of at most `sample_full_trial_max` bytes this is the full trial above;
/// at effort 1 only zstd runs (as `CodecChoice::Zstd`); at efforts 2..=4 the filter and the codec
/// are decided on `sample(data)` (`decide_on_sample`) and only the winner runs on the whole block.
/// Every path produces only codec ids, parameters and filters the full trial could have produced.
pub fn compress_best(data: &[u8], o: &CodecOptions) -> Result<Encoded> {
    if o.choice == CodecChoice::Best && o.effort.clamp(1, 5) == 1 {
        // zstd only (zstd+dict in granular mode), exactly what `CodecChoice::Zstd` runs, at every block size
        let variants: Vec<(u8, CodecSet)> = filter_candidates(data).into_iter().map(|f| (f, CodecSet::Only(&[ZSTD, ZSTD_DICT]))).collect();
        return compress_trial(data, o, &variants);
    }
    if !samples_block(data.len(), o) {
        // the full trial (effort 5, another choice, or a small block): the unchanged code path
        let variants: Vec<(u8, CodecSet)> = filter_candidates(data).into_iter().map(|f| (f, CodecSet::ByChoice)).collect();
        return compress_trial(data, o, &variants);
    }
    let d = decide_on_sample(&sample(data), o)?;
    let run: Vec<u8> = std::iter::once(d.codec).chain(d.also).collect();
    compress_trial(data, o, &[(d.filter, CodecSet::Only(&run))])
}

pub fn decompress(codec: u8, data: &[u8], ulen: usize, dict: Option<&[u8]>) -> Result<Vec<u8>> {
    decompress_with(codec, data, ulen, dict, &[], FILTER_NONE)
}

pub fn decompress_with(codec: u8, data: &[u8], ulen: usize, dict: Option<&[u8]>, params: &[u32], filter: u8) -> Result<Vec<u8>> {
    let out = decompress_inner(codec, data, ulen, dict, params)?;
    if filter == FILTER_NONE {
        return Ok(out);
    }
    apply_filter(filter, &out, false)
}

fn decompress_inner(codec: u8, data: &[u8], ulen: usize, dict: Option<&[u8]>, params: &[u32]) -> Result<Vec<u8>> {
    let out = match codec {
        PPMD => {
            if params.len() != 2 {
                return Err(Error::Corrupt("ppmd blob without parameters".into()));
            }
            let (order, mem) = (params[0], params[1]);
            if mem > (1 << 30) {
                return Err(Error::Limit(format!("ppmd memory {mem} exceeds the reader limit")));
            }
            ppmd_decompress(data, ulen, order, mem)?
        }
        STORE => data.to_vec(),
        ZSTD => {
            let mut d = zstd::bulk::Decompressor::new()?;
            d.set_parameter(DParameter::WindowLogMax(31))?;
            d.decompress(data, ulen)?
        }
        ZSTD_DICT => {
            let dict = dict.ok_or_else(|| Error::Missing("dictionary required by blob".into()))?;
            let mut d = zstd::bulk::Decompressor::with_dictionary(dict)?;
            d.set_parameter(DParameter::WindowLogMax(31))?;
            d.decompress(data, ulen)?
        }
        ZSTD_DELTA => return Err(Error::Corrupt("delta blob needs its dictionary chunks (reader path)".into())),
        XZ => {
            let stream = liblzma::stream::Stream::new_stream_decoder(1 << 30, 0).map_err(|e| Error::Encoding(format!("xz decoder: {e}")))?;
            let mut dec = liblzma::read::XzDecoder::new_stream(data, stream);
            let mut out = Vec::with_capacity(ulen);
            // bounded read: never allocate more than the declared length + 1 (detects overlong streams)
            let mut limited = (&mut dec).take(ulen as u64 + 1);
            limited.read_to_end(&mut out)?;
            out
        }
        other => return Err(Error::Corrupt(format!("unknown codec id {other}"))),
    };
    if out.len() != ulen {
        return Err(Error::Corrupt(format!("blob length {} != declared {}", out.len(), ulen)));
    }
    Ok(out)
}

/// Train a zstd dictionary on the unique chunks (needs at least 8 samples).
pub fn train_dict(samples: &[&[u8]], max_size: usize) -> Option<Vec<u8>> {
    if samples.len() < 8 {
        return None;
    }
    zstd::dict::from_samples(samples, max_size).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xz_roundtrip_and_best_choice() {
        let text: Vec<u8> = "the quick brown fox jumps over the lazy dog; ".repeat(2000).into_bytes();
        let e = compress_best(&text, &CodecOptions { choice: CodecChoice::Xz, ..Default::default() }).unwrap();
        assert_eq!(e.codec, XZ);
        assert_eq!(decompress(XZ, &e.bytes, text.len(), None).unwrap(), text);
        let b = compress_best(&text, &CodecOptions { choice: CodecChoice::Best, ..Default::default() }).unwrap();
        assert!(b.bytes.len() <= e.bytes.len());
        assert!(decompress(XZ, &e.bytes[..e.bytes.len() - 4], text.len(), None).is_err(), "truncated xz must fail");
    }

    #[test]
    fn x86_filter_roundtrips_and_helps_on_code() {
        // pseudo machine code: many E8 calls with small relative offsets
        let mut code = Vec::new();
        let mut x = 0x1234_5678u32;
        for i in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if i % 7 == 0 {
                code.push(0xE8);
                let rel = x % 4096;
                code.extend_from_slice(&rel.to_le_bytes());
            } else {
                code.push((x >> 24) as u8);
            }
        }
        assert!(looks_like_x86(&code));
        let mut enc = code.clone();
        let mut st = 0;
        x86_convert(&mut enc, 0, &mut st, true);
        assert_ne!(enc, code);
        let mut dec = enc.clone();
        let mut st2 = 0;
        x86_convert(&mut dec, 0, &mut st2, false);
        assert_eq!(dec, code, "x86 filter must be exactly invertible");
        let e = compress_best(&code, &CodecOptions { choice: CodecChoice::Zstd, ..Default::default() }).unwrap();
        assert_eq!(decompress_with(e.codec, &e.bytes, code.len(), None, &e.params, e.filter).unwrap(), code);
        // random data: never filtered, stored
        let mut y = 0x2545_F491u32;
        let rnd: Vec<u8> = (0..50_000)
            .map(|_| {
                y ^= y << 13;
                y ^= y >> 17;
                y ^= y << 5;
                (y >> 24) as u8
            })
            .collect();
        assert!(!looks_like_x86(&rnd) && arm64_alignment(&rnd).is_none());
        let r = compress_best(&rnd, &CodecOptions::default()).unwrap();
        assert_eq!(r.codec, STORE);
        assert_eq!(decompress_with(r.codec, &r.bytes, rnd.len(), None, &r.params, r.filter).unwrap(), rnd);
    }

    #[test]
    fn branch_filters_roundtrip_on_adversarial_inputs() {
        // Dense E8/E9 with 00/FF bytes everywhere to exercise every mask path of the x86 converter,
        // and dense BL/ADRP-looking words for ARM64; many sizes including unaligned tails.
        let mut x = 0x9E37_79B9u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for round in 0..300 {
            let len = 5 + (next() % 3000) as usize + if round % 3 == 0 { 4096 } else { 0 };
            let mut buf = vec![0u8; len];
            for b in buf.iter_mut() {
                let r = next();
                *b = match r % 6 {
                    0 => 0xE8,
                    1 => 0xE9,
                    2 => 0x00,
                    3 => 0xFF,
                    _ => (r >> 8) as u8,
                };
            }
            for &f in &[FILTER_X86, FILTER_ARM64, FILTER_ARM64 | 0x10, FILTER_ARM64 | 0x20, FILTER_ARM64 | 0x30] {
                let enc = apply_filter(f, &buf, true).unwrap();
                let dec = apply_filter(f, &enc, false).unwrap();
                assert_eq!(dec, buf, "filter {f} round {round} len {len}");
            }
            assert!(apply_filter(0x0F, &buf, true).is_err());
            let mut words = Vec::with_capacity(len);
            for i in 0..len / 4 {
                let r = next();
                let w = match i % 4 {
                    0 => 0x9400_0000 | (r & 0x0000_FFFF), // BL, small forward displacement
                    1 => 0x97FF_0000 | (r & 0x0000_FFFF), // BL, small backward displacement
                    2 => 0x9000_0000 | (r & 0x60FF_FFFF), // ADRP
                    _ => r,
                };
                words.extend_from_slice(&w.to_le_bytes());
            }
            let enc = apply_filter(FILTER_ARM64, &words, true).unwrap();
            assert_ne!(enc, words);
            assert_eq!(apply_filter(FILTER_ARM64, &enc, false).unwrap(), words);
            // a misaligned copy of code must be detected at its alignment
            let mut shifted = vec![0xAAu8; 3];
            shifted.extend_from_slice(&words);
            if words.len() >= 4096 {
                assert_eq!(arm64_alignment(&shifted), Some(3), "round {round}");
            }
        }
    }

    #[test]
    fn ppmd_roundtrip() {
        let mut text = String::new();
        for i in 0..4000 {
            text.push_str(&format!("Paragraph {i}: content-defined chunking keeps identical data identical; PPMd models text well.\n"));
        }
        let data = text.into_bytes();
        assert!(looks_like_text(&data));
        let e = compress_best(&data, &CodecOptions { choice: CodecChoice::Ppmd, ..Default::default() }).unwrap();
        assert_eq!(e.codec, PPMD);
        assert_eq!(e.params.len(), 2);
        assert_eq!(decompress_with(PPMD, &e.bytes, data.len(), None, &e.params, FILTER_NONE).unwrap(), data);
        assert!(decompress_with(PPMD, &e.bytes, data.len(), None, &[], FILTER_NONE).is_err());
        let z = compress_best(&data, &CodecOptions { choice: CodecChoice::Zstd, ..Default::default() }).unwrap();
        println!("ppmd {} vs zstd {}", e.bytes.len(), z.bytes.len());
    }
}
