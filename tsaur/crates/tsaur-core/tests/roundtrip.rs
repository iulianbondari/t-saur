//! End-to-end tests: round trip, determinism, corruption detection, encryption, signatures,
//! range reads, container awareness, pieces + parity recovery.

use std::io::Write;
use std::path::{Path, PathBuf};
use tsaur_core::chunk::ChunkParams;
use tsaur_core::crypto::KdfParams;
use tsaur_core::{pack, pieces, PackOptions, Reader};

struct Fixture {
    dir: PathBuf,
    files: Vec<PathBuf>,
}

fn xorshift(seed: &mut u64) -> u64 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    x
}

fn fixture(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("tsaur-test-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    // 1. repetitive text with variation (compressible, dedup-friendly)
    let mut text = String::new();
    let words = ["archive", "agent", "chunk", "merkle", "tsaur", "verify", "content", "reference", "dictionary", "solid"];
    let mut seed = 0x1234_5678_9abc_def0u64;
    for i in 0..40_000 {
        let w = words[(xorshift(&mut seed) % words.len() as u64) as usize];
        text.push_str(w);
        text.push(if i % 13 == 0 { '\n' } else { ' ' });
    }
    let text_path = dir.join("text.txt");
    std::fs::write(&text_path, &text).unwrap();
    // 2. edited copy of the text (dedup should kick in)
    let mut text2 = text.clone();
    text2.insert_str(5000, "INSERTED PARAGRAPH\n");
    text2.replace_range(100_000..100_010, "XXXXXXXXXX");
    let text2_path = dir.join("sub").join("text-v2.txt");
    std::fs::create_dir_all(text2_path.parent().unwrap()).unwrap();
    std::fs::write(&text2_path, &text2).unwrap();
    // 3. random bytes (incompressible)
    let mut rnd = vec![0u8; 200_000];
    for b in rnd.iter_mut() {
        *b = (xorshift(&mut seed) >> 24) as u8;
    }
    let rnd_path = dir.join("random.bin");
    std::fs::write(&rnd_path, &rnd).unwrap();
    // 4. a small zip container with deflated members
    let zip_path = dir.join("doc.zip");
    {
        let f = std::fs::File::create(&zip_path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("word/document.xml", o).unwrap();
        w.write_all(&text.as_bytes()[..50_000]).unwrap();
        w.start_file("word/styles.xml", o).unwrap();
        w.write_all(b"<styles>plain</styles>".repeat(400).as_slice()).unwrap();
        let o2 = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("media/blob.bin", o2).unwrap();
        w.write_all(&rnd[..5000]).unwrap();
        w.finish().unwrap();
    }
    // 5. empty file and tiny file
    std::fs::write(dir.join("empty.txt"), b"").unwrap();
    std::fs::write(dir.join("tiny.md"), b"# tiny\n").unwrap();
    // 6. a PDF-like file with zlib FlateDecode streams (precomp-style inversion)
    let pdf_path = dir.join("doc.pdf");
    {
        use flate2::write::ZlibEncoder;
        let mut pdf: Vec<u8> = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        for (i, slice) in [&text[..30_000], &text[10_000..45_000], &text2[..20_000]].iter().enumerate() {
            let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(slice.as_bytes()).unwrap();
            let z = enc.finish().unwrap();
            pdf.extend_from_slice(format!("{} 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n", i + 2, z.len()).as_bytes());
            pdf.extend_from_slice(&z);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        std::fs::write(&pdf_path, &pdf).unwrap();
    }
    Fixture { dir: dir.clone(), files: vec![text_path, text2_path, rnd_path, zip_path, dir.join("empty.txt"), dir.join("tiny.md"), pdf_path] }
}

fn assert_extracted_equal(fx: &Fixture, out_dir: &Path) {
    for f in &fx.files {
        let rel = f.strip_prefix(&fx.dir).unwrap();
        let restored = out_dir.join(rel);
        assert_eq!(std::fs::read(f).unwrap(), std::fs::read(&restored).unwrap_or_else(|_| panic!("missing {}", restored.display())), "mismatch for {}", rel.display());
    }
}

#[test]
fn granular_roundtrip_with_dict_and_dedup() {
    let fx = fixture("granular");
    let out = fx.dir.join("a.tsr");
    let report = pack(std::slice::from_ref(&fx.dir), &out, PackOptions { chunk: ChunkParams::FINE, block_size: 0, dict_warmup: 64 * 1024, ..Default::default() }).unwrap();
    assert!(report.chunks_unique < report.chunks_total, "dedup expected between text.txt and text-v2.txt");
    assert!(report.dict_bytes > 0, "granular mode should train a dictionary after the warm-up window");
    assert!(report.blobs == report.chunks_unique, "granular mode: one blob per unique chunk");
    assert!(report.archive_bytes < report.input_bytes);
    let mut r = Reader::open(&out, None).unwrap();
    let rep = r.verify(None).unwrap();
    assert!(rep.entries_bad.is_empty(), "{:?}", rep.entries_bad);
    let ex = fx.dir.join("out-granular");
    r.extract(&ex, None, false).unwrap();
    assert_extracted_equal(&fx, &ex);
    let zip_entry = r.entries().iter().find(|e| e.path == "doc.zip").unwrap();
    assert_eq!(zip_entry.mode, "zip", "container should be exploded and rebuilt bit-exact");
    let pdf_entry = r.entries().iter().find(|e| e.path == "doc.pdf").unwrap();
    // miniz-produced zlib streams may or may not be modelled by preflate; either way the round trip is exact
    println!("doc.pdf mode = {} ({:?})", pdf_entry.mode, pdf_entry.note);
    assert!(pdf_entry.mode == "streams" || pdf_entry.mode == "raw");
}

#[test]
fn solid_encrypted_signed_roundtrip() {
    let fx = fixture("solid");
    let out = fx.dir.join("b.tsr");
    let (seed, pubkey) = tsaur_core::crypto::sig::keygen().unwrap();
    let opts = PackOptions {
        chunk: ChunkParams::FINE,
        block_size: 1 << 20,
        password: Some("correct horse".into()),
        kdf: KdfParams { m_kib: 64 * 1024, t: 3, p: 4 },
        sign_seed: Some(seed),
        ..Default::default()
    };
    let report = pack(std::slice::from_ref(&fx.dir), &out, opts).unwrap();
    assert!(report.encrypted && report.signed);
    assert!(Reader::open(&out, None).is_err(), "must require a password");
    assert!(Reader::open(&out, Some("wrong")).is_err(), "wrong password must fail");
    let mut r = Reader::open(&out, Some("correct horse")).unwrap();
    assert!(r.verify_signature(&pubkey).unwrap());
    let other = tsaur_core::crypto::sig::keygen().unwrap().1;
    assert!(!r.verify_signature(&other).unwrap());
    let ex = fx.dir.join("out-solid");
    r.extract(&ex, None, false).unwrap();
    assert_extracted_equal(&fx, &ex);
}

#[test]
fn deterministic_output() {
    let fx = fixture("determinism");
    let a = fx.dir.join("d1.tsr");
    let b = fx.dir.join("d2.tsr");
    let opts = PackOptions { chunk: ChunkParams::FINE, block_size: 1 << 20, ..Default::default() };
    pack(&[fx.dir.join("text.txt"), fx.dir.join("random.bin"), fx.dir.join("doc.zip")], &a, opts.clone()).unwrap();
    pack(&[fx.dir.join("text.txt"), fx.dir.join("random.bin"), fx.dir.join("doc.zip")], &b, opts).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "same inputs + options must give identical bytes");
}

#[test]
fn every_effort_roundtrips_and_is_deterministic() {
    let mut fx = fixture("effort");
    // a 1.2 MiB prose file, so that at least one block exceeds the 128 KiB full-trial threshold
    // and is decided on a sample at efforts 2..4
    let words = ["archive", "agent", "chunk", "merkle", "verify", "content", "reference", "dictionary", "solid", "the", "of", "a"];
    let mut seed = 0x5eed_0000_0000_0001u64;
    let mut big = String::new();
    while big.len() < 1_200_000 {
        big.push_str(words[(xorshift(&mut seed) % words.len() as u64) as usize]);
        big.push(if xorshift(&mut seed).is_multiple_of(12) { '\n' } else { ' ' });
    }
    let big_path = fx.dir.join("big.txt");
    std::fs::write(&big_path, &big).unwrap();
    fx.files.push(big_path);
    // archives and extractions live next to the inputs, not inside them: the inputs must be the
    // same for every pack
    let work = PathBuf::from(format!("{}-work", fx.dir.display()));
    std::fs::create_dir_all(&work).unwrap();
    let mut sizes = std::collections::BTreeMap::new();
    let mut bytes5 = Vec::new();
    for effort in 1..=5u8 {
        let opts = PackOptions { effort, ..Default::default() };
        let a = work.join(format!("e{effort}-a.tsr"));
        let b = work.join(format!("e{effort}-b.tsr"));
        let ra = pack(std::slice::from_ref(&fx.dir), &a, opts.clone()).unwrap();
        let rb = pack(std::slice::from_ref(&fx.dir), &b, opts).unwrap();
        let bytes = std::fs::read(&a).unwrap();
        assert_eq!(bytes, std::fs::read(&b).unwrap(), "effort {effort}: same inputs + options must give identical bytes");
        assert_eq!(ra.blocks_sampled, rb.blocks_sampled);
        if matches!(effort, 1 | 5) {
            assert_eq!(ra.blocks_sampled, 0, "effort {effort} never samples");
        } else {
            assert!(ra.blocks_sampled > 0, "effort {effort}: the 1.2 MiB file must give at least one sampled block ({:?})", ra.codec_hist);
        }
        let mut r = Reader::open(&a, None).unwrap();
        assert!(r.verify(None).unwrap().entries_bad.is_empty());
        let ex = work.join(format!("out-e{effort}"));
        r.extract(&ex, None, false).unwrap();
        assert_extracted_equal(&fx, &ex);
        sizes.insert(effort, ra.archive_bytes);
        if effort == 5 {
            bytes5 = bytes;
        }
    }
    // the default is effort 5 and the effort is not archive content: nothing but the codec choice differs
    let d = work.join("default.tsr");
    let rd = pack(std::slice::from_ref(&fx.dir), &d, PackOptions::default()).unwrap();
    assert_eq!(std::fs::read(&d).unwrap(), bytes5, "PackOptions::default() must be effort 5");
    assert_eq!(rd.blocks_sampled, 0);
    assert!(sizes[&3] <= sizes[&1], "effort 3 must not lose to zstd only: {sizes:?}");
    println!("archive bytes per effort: {sizes:?}");
    let _ = std::fs::remove_dir_all(&fx.dir);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn corruption_is_detected() {
    let fx = fixture("corrupt");
    let out = fx.dir.join("c.tsr");
    pack(&[fx.dir.join("text.txt")], &out, PackOptions { chunk: ChunkParams::FINE, ..Default::default() }).unwrap();
    let mut bytes = std::fs::read(&out).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x5a;
    std::fs::write(&out, &bytes).unwrap();
    let res = Reader::open(&out, None);
    assert!(res.is_err(), "a flipped byte must be caught by the section hashes");
}

#[test]
fn range_reads() {
    let fx = fixture("range");
    let out = fx.dir.join("r.tsr");
    pack(&[fx.dir.join("text.txt")], &out, PackOptions { chunk: ChunkParams::FINE, ..Default::default() }).unwrap();
    let original = std::fs::read(fx.dir.join("text.txt")).unwrap();
    let mut r = Reader::open(&out, None).unwrap();
    let ei = r.find_entry("text.txt").unwrap();
    for (start, len) in [(0u64, 10usize), (7000, 20_000), (original.len() as u64 - 5, 100), (123_456, 1)] {
        let got = r.read_range(ei, start, len).unwrap();
        let s = start as usize;
        let e = (s + len).min(original.len());
        assert_eq!(got, original[s..e], "range {start}+{len}");
    }
}

#[test]
fn pieces_and_recovery() {
    let fx = fixture("pieces");
    let out = fx.dir.join("p.tsr");
    pack(std::slice::from_ref(&fx.dir), &out, PackOptions { chunk: ChunkParams::FINE, block_size: 1 << 20, ..Default::default() }).unwrap();
    let original = std::fs::read(&out).unwrap();
    let meta = pieces::write_sidecars(&out, 16 * 1024, 0.15).unwrap();
    assert!(meta.parity.len() >= 2);
    // destroy as many pieces as the first stripe's parity allows
    let m = meta.stripes[0].m as usize;
    let mut bytes = original.clone();
    for j in 0..m {
        let start = j * 16 * 1024 * 3 / 2; // spread the damage
        let start = start.min(bytes.len() - 16);
        for b in &mut bytes[start..start + 16] {
            *b = !*b;
        }
    }
    std::fs::write(&out, &bytes).unwrap();
    let (bad, _) = pieces::verify(&out).unwrap();
    assert!(!bad.is_empty() && bad.len() <= m, "bad pieces: {bad:?}");
    let (fixed, ok) = pieces::recover(&out).unwrap();
    assert_eq!(fixed as usize, bad.len());
    assert!(ok);
    assert_eq!(std::fs::read(&out).unwrap(), original, "recovered archive must be bit-identical");
}

#[test]
fn block_mode_default_roundtrip_and_small_blocks() {
    // default options (64 KiB chunks, 1 MiB blocks) and tiny 64 KiB blocks: many blobs, all verified
    let fx = fixture("blocks");
    for (name, bs) in [("d1.tsr", 1u32 << 20), ("d2.tsr", 64 << 10)] {
        let out = fx.dir.join(name);
        let rep = pack(std::slice::from_ref(&fx.dir), &out, PackOptions { block_size: bs, ..Default::default() }).unwrap();
        assert!(rep.blobs >= 1);
        let mut r = Reader::open(&out, None).unwrap();
        let ex = fx.dir.join(format!("out-{name}"));
        r.extract(&ex, None, false).unwrap();
        assert_extracted_equal(&fx, &ex);
        if bs == 64 << 10 {
            assert!(rep.blobs > 5, "64 KiB blocks should produce many blobs, got {}", rep.blobs);
        }
    }
}

#[test]
fn random_corruption_never_panics_and_is_detected() {
    // Flip bytes at pseudo-random positions (framing, index, manifest, blobs): the reader must
    // return an error, never panic, and never extract wrong bytes.
    let fx = fixture("fuzz");
    let out = fx.dir.join("f.tsr");
    pack(&[fx.dir.join("text.txt"), fx.dir.join("doc.zip"), fx.dir.join("tiny.md")], &out, PackOptions { chunk: ChunkParams::FINE, block_size: 64 << 10, ..Default::default() }).unwrap();
    let original = std::fs::read(&out).unwrap();
    let mut seed = 0xdead_beef_cafe_f00du64;
    let mut detected = 0;
    for _ in 0..150 {
        let mut bytes = original.clone();
        let pos = (xorshift(&mut seed) % bytes.len() as u64) as usize;
        let flips = 1 + (xorshift(&mut seed) % 4) as usize;
        for k in 0..flips {
            let p = (pos + k * 7) % bytes.len();
            bytes[p] ^= (1 + (xorshift(&mut seed) % 255)) as u8;
        }
        let res = std::panic::catch_unwind(|| {
            let r = Reader::from_bytes(bytes.clone(), None);
            match r {
                Err(_) => true,
                Ok(mut reader) => {
                    // a corruption that survives the section hashes is impossible; if the archive opens,
                    // every entry must still verify
                    let rep = reader.verify(None).unwrap();
                    rep.entries_bad.is_empty()
                }
            }
        });
        match res {
            Ok(true) => detected += 1,
            Ok(false) => panic!("corruption survived verification"),
            Err(_) => panic!("reader panicked on corrupt input"),
        }
    }
    assert_eq!(detected, 150);
    // truncations at every 1/16 of the file
    for i in 1..16 {
        let cut = original.len() * i / 16;
        let res = std::panic::catch_unwind(|| Reader::from_bytes(original[..cut].to_vec(), None).is_err());
        assert_eq!(res.ok(), Some(true), "truncated archive at {cut} must be rejected without panic");
    }
}

#[test]
fn reference_archive_stores_only_new_chunks() {
    let fx = fixture("refs");
    let base = fx.dir.join("base.tsr");
    let inc = fx.dir.join("inc.tsr");
    let opts = PackOptions { chunk: ChunkParams::FINE, ..Default::default() };
    pack(&[fx.dir.join("text.txt"), fx.dir.join("random.bin")], &base, opts.clone()).unwrap();
    let rep = pack(&[fx.dir.join("sub"), fx.dir.join("tiny.md"), fx.dir.join("random.bin")], &inc, PackOptions { references: vec![base.clone()], ..opts.clone() }).unwrap();
    assert!(rep.referenced_chunks > 0, "text-v2 and random.bin share chunks with base");
    assert!(rep.referenced_bytes > 150_000, "random.bin (200 KB) must be referenced, got {}", rep.referenced_bytes);
    assert!(rep.archive_bytes < 60_000, "incremental archive should be small, got {}", rep.archive_bytes);
    // without the reference: opens, but external chunks are missing -> verify reports errors, no panic
    let mut alone = Reader::open(&inc, None).unwrap();
    assert!(alone.unresolved_external() > 0);
    let vr = alone.verify(None).unwrap();
    assert!(!vr.entries_bad.is_empty());
    // with the reference: everything resolves and extracts bit-exact
    let mut r = Reader::open_with_refs(&inc, None, std::slice::from_ref(&base)).unwrap();
    assert_eq!(r.unresolved_external(), 0);
    let vr = r.verify(None).unwrap();
    assert!(vr.entries_bad.is_empty(), "{:?}", vr.entries_bad);
    let ex = fx.dir.join("out-inc");
    r.extract(&ex, None, false).unwrap();
    assert_eq!(std::fs::read(fx.dir.join("random.bin")).unwrap(), std::fs::read(ex.join("random.bin")).unwrap());
    assert_eq!(std::fs::read(fx.dir.join("sub").join("text-v2.txt")).unwrap(), std::fs::read(ex.join("text-v2.txt")).unwrap());
}

#[test]
fn rejects_unsafe_paths_on_extract() {
    // an entry path is validated on the way in, so packing a weird name must fail early
    let fx = fixture("paths");
    let out = fx.dir.join("x.tsr");
    let res = pack(&[fx.dir.join("nonexistent")], &out, PackOptions::default());
    assert!(res.is_err());
    assert!(tsaur_core::paths::normalize("../evil").is_err());
    assert!(tsaur_core::paths::normalize("ok/evil:ads").is_err());
}

/// A 32x24 progressive JPEG (573 bytes) generated with PyMuPDF; used to exercise the Lepton path.
#[cfg(feature = "lepton")]
const TINY_JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x01, 0x00, 0x60, 0x00, 0x60, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x06, 0x04, 0x05, 0x06, 0x05, 0x04, 0x06,
    0x06, 0x05, 0x06, 0x07, 0x07, 0x06, 0x08, 0x0a, 0x10, 0x0a, 0x0a, 0x09, 0x09, 0x0a, 0x14, 0x0e, 0x0f, 0x0c, 0x10, 0x17, 0x14, 0x18, 0x18, 0x17, 0x14, 0x16, 0x16, 0x1a, 0x1d, 0x25, 0x1f, 0x1a,
    0x1b, 0x23, 0x1c, 0x16, 0x16, 0x20, 0x2c, 0x20, 0x23, 0x26, 0x27, 0x29, 0x2a, 0x29, 0x19, 0x1f, 0x2d, 0x30, 0x2d, 0x28, 0x30, 0x25, 0x28, 0x29, 0x28, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x06, 0x07,
    0x07, 0x0a, 0x08, 0x0a, 0x13, 0x0a, 0x0a, 0x13, 0x28, 0x1a, 0x16, 0x1a, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28,
    0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0xff, 0xc2,
    0x00, 0x11, 0x08, 0x00, 0x18, 0x00, 0x20, 0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xc4, 0x00, 0x17, 0x00, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01, 0x03, 0x05, 0xff, 0xc4, 0x00, 0x19, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
    0x01, 0x00, 0x03, 0x05, 0x07, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x10, 0x03, 0x10, 0x00, 0x00, 0x01, 0xef, 0x7c, 0xfb, 0xcb, 0xb1, 0xac, 0x94, 0x78, 0xbe, 0x0a, 0x2b, 0x9a, 0x8b,
    0x17, 0xc1, 0x47, 0x73, 0x51, 0x7f, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0xff, 0xda, 0x00, 0x08,
    0x01, 0x01, 0x00, 0x01, 0x05, 0x02, 0x4f, 0xff, 0xc4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0xff, 0xda, 0x00,
    0x08, 0x01, 0x03, 0x01, 0x01, 0x3f, 0x01, 0x4f, 0xff, 0xc4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0xff, 0xda,
    0x00, 0x08, 0x01, 0x02, 0x01, 0x01, 0x3f, 0x01, 0x4f, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0xff,
    0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x06, 0x3f, 0x02, 0x4f, 0xff, 0xc4, 0x00, 0x17, 0x10, 0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x10, 0x00, 0x20, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x01, 0x3f, 0x21, 0x63, 0x42, 0x0b, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x00, 0x00, 0x10, 0x78, 0xb8,
    0xe7, 0x2a, 0x3f, 0xff, 0xc4, 0x00, 0x15, 0x11, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x03,
    0x01, 0x01, 0x3f, 0x10, 0x22, 0x22, 0x22, 0x22, 0x08, 0x20, 0x82, 0xff, 0xc4, 0x00, 0x16, 0x11, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x11, 0x20, 0xff, 0xda, 0x00, 0x08, 0x01, 0x02, 0x01, 0x01, 0x3f, 0x10, 0xdc, 0x44, 0x44, 0x7f, 0xff, 0xc4, 0x00, 0x16, 0x10, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x10, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x01, 0x3f, 0x10, 0x22, 0x30, 0x30, 0xc3, 0x0c, 0x32, 0x94, 0x37, 0xff, 0xd9,
];

#[test]
#[cfg(feature = "lepton")]
fn jpeg_files_and_dct_streams_roundtrip() {
    let fx = fixture("jpeg");
    std::fs::write(fx.dir.join("tiny.jpg"), TINY_JPEG).unwrap();
    // a PDF-like file with the JPEG as a DCTDecode stream
    let mut pdf: Vec<u8> = b"%PDF-1.7
5 0 obj
<< /Type /XObject /Subtype /Image /Filter /DCTDecode >>
stream
"
    .to_vec();
    pdf.extend_from_slice(TINY_JPEG);
    pdf.extend_from_slice(
        b"
endstream
endobj
trailer
<< /Root 1 0 R >>
%%EOF
",
    );
    std::fs::write(fx.dir.join("img.pdf"), &pdf).unwrap();
    let out = fx.dir.join("j.tsr");
    pack(&[fx.dir.join("tiny.jpg"), fx.dir.join("img.pdf")], &out, PackOptions::default()).unwrap();
    let mut r = Reader::open(&out, None).unwrap();
    for e in r.entries() {
        println!("{} mode={} note={:?}", e.path, e.mode, e.note);
        assert!(e.mode == "streams" || e.mode == "raw");
    }
    let rep = r.verify(None).unwrap();
    assert!(rep.entries_bad.is_empty(), "{:?}", rep.entries_bad);
    let ex = fx.dir.join("out-jpeg");
    r.extract(&ex, None, false).unwrap();
    assert_eq!(std::fs::read(ex.join("tiny.jpg")).unwrap(), TINY_JPEG);
    assert_eq!(std::fs::read(ex.join("img.pdf")).unwrap(), pdf);
    // direct Lepton round trip on the fixture (independent of whether it came out smaller)
    if let Some(lep) = tsaur_core::container::jpeg_to_lepton(TINY_JPEG) {
        assert_eq!(tsaur_core::container::lepton_to_jpeg(&lep, TINY_JPEG.len() as u64).unwrap(), TINY_JPEG);
    }
}

#[test]
fn hybrid_pq_recipients_roundtrip() {
    use tsaur_core::crypto::{hybrid::Identity, Credentials};
    let fx = fixture("hybrid");
    let alice = Identity::generate().unwrap();
    let bob = Identity::generate().unwrap();
    let mallory = Identity::generate().unwrap();
    // identity survives a byte round trip
    let alice2 = Identity::from_bytes(&alice.to_bytes()).unwrap();
    assert_eq!(alice.recipient(), alice2.recipient());
    let out = fx.dir.join("h.tsr");
    let rep =
        pack(&[fx.dir.join("text.txt"), fx.dir.join("doc.zip")], &out, PackOptions { chunk: ChunkParams::FINE, recipients: vec![alice.recipient(), bob.recipient()], ..Default::default() }).unwrap();
    assert!(rep.encrypted);
    // no credentials / wrong identity / password only -> refused
    assert!(Reader::open(&out, None).is_err());
    assert!(Reader::open_with(&out, &Credentials { password: None, identities: vec![mallory.clone()] }, &[]).is_err());
    assert!(Reader::open(&out, Some("pw")).is_err());
    // either recipient opens it, also when presented together with a wrong one
    for id in [alice.clone(), bob.clone()] {
        let mut r = Reader::open_with(&out, &Credentials { password: None, identities: vec![mallory.clone(), id] }, &[]).unwrap();
        let v = r.verify(None).unwrap();
        assert!(v.entries_bad.is_empty());
        assert_eq!(r.entry_bytes(r.find_entry("text.txt").unwrap()).unwrap(), std::fs::read(fx.dir.join("text.txt")).unwrap());
    }
    // password + recipient together: both work
    let out2 = fx.dir.join("h2.tsr");
    pack(&[fx.dir.join("tiny.md")], &out2, PackOptions { password: Some("secret".into()), kdf: KdfParams { m_kib: 64 * 1024, t: 3, p: 4 }, recipients: vec![bob.recipient()], ..Default::default() })
        .unwrap();
    assert!(Reader::open(&out2, Some("secret")).is_ok());
    assert!(Reader::open_with(&out2, &Credentials { password: None, identities: vec![bob] }, &[]).is_ok());
    assert!(Reader::open_with(&out2, &Credentials { password: Some("wrong".into()), identities: vec![alice] }, &[]).is_err());
}

#[test]
fn pieces_streaming_multi_stripe_recovery() {
    // ~1.5 MB of incompressible data -> 1024-byte pieces -> several 200-piece stripes; damage in
    // three stripes plus truncation, then garbage appended, then more damage than parity.
    let dir = std::env::temp_dir().join(format!("tsaur-test-stripes-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 0xdead_beef_cafe_f00du64;
    let blob: Vec<u8> = (0..1_500_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(dir.join("big.bin"), &blob).unwrap();
    let out = dir.join("s.tsr");
    pack(&[dir.join("big.bin")], &out, PackOptions::default()).unwrap();
    let original = std::fs::read(&out).unwrap();
    let meta = pieces::write_sidecars(&out, 1024, 0.10).unwrap();
    assert!(meta.stripes.len() >= 7, "expected several stripes, got {}", meta.stripes.len());
    assert!(meta.stripes.iter().all(|s| s.k <= 200 && s.m == (s.k as f64 * 0.10).round() as u32));

    // 1. damage 3 pieces in stripe 0, 5 in stripe 3 and truncate the tail (stripe 7)
    let mut bytes = original.clone();
    for &p in &[1usize, 50, 199, 601, 650, 700, 750, 799] {
        bytes[p * 1024 + 10] ^= 0xFF;
    }
    bytes.truncate(original.len() - 3000);
    std::fs::write(&out, &bytes).unwrap();
    let (bad, _) = pieces::verify(&out).unwrap();
    assert!(bad.len() >= 8 + 2, "bad: {bad:?}");
    let (fixed, ok) = pieces::recover(&out).unwrap();
    assert!(ok, "recovery must succeed within parity");
    assert_eq!(fixed as usize, bad.len());
    assert_eq!(std::fs::read(&out).unwrap(), original);
    let mut r = Reader::open(&out, None).unwrap();
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
    drop(r); // the reader maps the file; Windows refuses to rewrite a mapped file

    // 2. garbage appended after the archive: flagged and removed
    let mut longer = original.clone();
    longer.extend_from_slice(b"trailing junk that breaks the trailer");
    std::fs::write(&out, &longer).unwrap();
    assert!(Reader::open(&out, None).is_err());
    let (bad, _) = pieces::verify(&out).unwrap();
    assert_eq!(bad, vec![meta.pieces.len() as u32 - 1]);
    let (_, ok) = pieces::recover(&out).unwrap();
    assert!(ok);
    assert_eq!(std::fs::read(&out).unwrap(), original);

    // 3. more damage than parity in stripe 1, damage within parity in stripe 2: stripe 2 is still repaired
    let mut bytes = original.clone();
    let m1 = meta.stripes[1].m as usize;
    for p in 200..200 + m1 + 1 {
        bytes[p * 1024 + 5] ^= 0x55;
    }
    bytes[450 * 1024 + 5] ^= 0x55;
    std::fs::write(&out, &bytes).unwrap();
    let (fixed, ok) = pieces::recover(&out).unwrap();
    assert!(!ok);
    assert_eq!(fixed, 1);
    let (bad_after, _) = pieces::verify(&out).unwrap();
    assert_eq!(bad_after.len(), m1 + 1);
    assert!(bad_after.iter().all(|&b| (200..200 + m1 as u32 + 1).contains(&b)));

    // 4. a missing archive: every piece is bad, the sidecar still loads
    std::fs::remove_file(&out).unwrap();
    let (bad, _) = pieces::verify(&out).unwrap();
    assert_eq!(bad.len(), meta.pieces.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hybrid_views_are_stored_and_served() {
    let fx = fixture("views");
    // a real (minimal) WordprocessingML document next to the fixture files
    let docx_path = fx.dir.join("report.docx");
    {
        let f = std::fs::File::create(&docx_path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("[Content_Types].xml", o).unwrap();
        w.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#).unwrap();
        w.start_file("word/document.xml", o).unwrap();
        w.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Quarterly report</w:t></w:r></w:p><w:p><w:r><w:t>Hello world, this is the body paragraph.</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        w.finish().unwrap();
    }
    let out = fx.dir.join("v.tsr");
    let rep = pack(std::slice::from_ref(&fx.dir), &out, PackOptions { canonical: true, ..Default::default() }).unwrap();
    assert!(rep.views >= 1, "the DOCX must produce a stored view");
    let mut r = Reader::open(&out, None).unwrap();
    assert_eq!(r.manifest.fidelity, "hybrid");
    let view = r.entries().iter().find(|e| e.derived.as_ref().is_some_and(|d| d.from == "report.docx")).cloned().expect("stored view of report.docx");
    let d = view.derived.clone().unwrap();
    assert_eq!(view.path, ".tsaur/views/report.docx.md");
    assert_eq!(d.view, "markdown");
    assert!(d.tokens_est > 0);
    let src = r.find_entry("report.docx").unwrap();
    let vi = r.find_entry(&view.path).unwrap();
    let text = String::from_utf8(r.entry_bytes(vi).unwrap()).unwrap();
    assert!(text.contains("canonical view"), "provenance header expected");
    assert!(text.contains("Hello world"));
    // identical to the on-demand conversion, and the original is still bit-exact
    let raw = r.entry_bytes(src).unwrap();
    assert_eq!(raw, std::fs::read(&docx_path).unwrap());
    assert_eq!(text, tsaur_core::canonical::canonical_view("report.docx", &raw).unwrap().unwrap());
    // default extraction skips views; explicit selection extracts them
    let ex = fx.dir.join("out");
    let written = r.extract(&ex, None, false).unwrap();
    assert!(written.iter().all(|p| !p.to_string_lossy().contains(".tsaur")));
    assert_extracted_equal(&fx, &ex);
    assert!(ex.join("report.docx").exists());
    let written = r.extract(&ex, Some(&[vi]), false).unwrap();
    assert_eq!(written.len(), 1);
    assert!(ex.join(".tsaur/views/report.docx.md").exists());
    // verify covers the views too
    let vr = r.verify(None).unwrap();
    assert!(vr.entries_bad.is_empty());
    assert_eq!(vr.entries_total, r.entries().len());
}

#[test]
fn corrupt_sidecars_never_panic_and_never_damage_the_archive() {
    let dir = std::env::temp_dir().join(format!("tsaur-test-sidecar-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 0x5151_7777_abcd_0123u64;
    let blob: Vec<u8> = (0..300_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(dir.join("big.bin"), &blob).unwrap();
    let out = dir.join("s.tsr");
    pack(&[dir.join("big.bin")], &out, PackOptions::default()).unwrap();
    pieces::write_sidecars(&out, 1024, 0.10).unwrap();
    let original = std::fs::read(&out).unwrap();
    let (pieces_path, par_path) = pieces::sidecar_paths(&out);
    let (meta_ok, par_ok) = (std::fs::read(&pieces_path).unwrap(), std::fs::read(&par_path).unwrap());
    for round in 0..80u64 {
        let mut meta = meta_ok.clone();
        let mut par = par_ok.clone();
        let r = xorshift(&mut seed);
        match round % 4 {
            0 => {
                let i = (r as usize) % meta.len();
                meta[i] ^= 1 << (r >> 8 & 7);
            }
            1 => {
                let n = (r as usize) % meta.len();
                meta.truncate(n);
            }
            2 => {
                let i = (r as usize) % par.len();
                par[i] ^= 0xFF;
            }
            _ => {
                let n = (r as usize) % par.len();
                par.truncate(n);
            }
        }
        std::fs::write(&pieces_path, &meta).unwrap();
        std::fs::write(&par_path, &par).unwrap();
        // errors are fine, panics are not; the archive itself must stay intact
        let _ = pieces::verify(&out);
        let _ = pieces::recover(&out);
        assert_eq!(std::fs::read(&out).unwrap(), original, "round {round}: a corrupt sidecar must never change the archive");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn case_colliding_entries_are_refused_on_case_insensitive_filesystems() {
    let dir = std::env::temp_dir().join(format!("tsaur-test-case-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(dir.join("one")).unwrap();
    std::fs::create_dir_all(dir.join("two")).unwrap();
    std::fs::write(dir.join("one/Notes.txt"), b"upper case\n").unwrap();
    std::fs::write(dir.join("two/notes.txt"), b"lower case\n").unwrap();
    let out = dir.join("c.tsr");
    pack(&[dir.join("one/Notes.txt"), dir.join("two/notes.txt")], &out, PackOptions::default()).unwrap();
    let mut r = Reader::open(&out, None).unwrap();
    assert_eq!(r.entries().len(), 2);
    let ex = dir.join("out");
    let res = r.extract(&ex, None, false);
    if cfg!(any(windows, target_os = "macos")) {
        assert!(matches!(res, Err(tsaur_core::Error::Policy(_))), "must refuse instead of overwriting: {res:?}");
        assert!(!ex.join("notes.txt").exists() && !ex.join("Notes.txt").exists(), "nothing may be written");
        // explicit selection still works
        let one = r.find_entry("Notes.txt").unwrap();
        r.extract(&ex, Some(&[one]), false).unwrap();
        assert_eq!(std::fs::read(ex.join("Notes.txt")).unwrap(), b"upper case\n");
    } else {
        assert_eq!(res.unwrap().len(), 2);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn long_paths_roundtrip() {
    // ~330 characters of nested directories: above the classic Windows MAX_PATH limit
    let dir = std::env::temp_dir().join(format!("tsaur-test-long-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let comp = "d".repeat(60);
    let rel = std::path::PathBuf::from(&comp).join(&comp).join(&comp).join(&comp).join(&comp).join("deep-file.txt");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join(&rel).parent().unwrap()).unwrap();
    std::fs::write(src.join(&rel), b"deep content\n").unwrap();
    let out = dir.join("l.tsr");
    pack(std::slice::from_ref(&src), &out, PackOptions::default()).unwrap();
    let mut r = Reader::open(&out, None).unwrap();
    assert!(r.entries()[0].path.len() > 300);
    let ex = dir.join("out");
    r.extract(&ex, None, false).unwrap();
    assert_eq!(std::fs::read(ex.join(&rel)).unwrap(), b"deep content\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// v1 = incompressible bytes; v2 = v1 with small edits spread over the file (a "new version").
fn versioned_pair(seed: &mut u64, len: usize, edits: usize) -> (Vec<u8>, Vec<u8>) {
    let v1: Vec<u8> = (0..len).map(|_| (xorshift(seed) >> 56) as u8).collect();
    let mut v2 = v1.clone();
    for k in 0..edits {
        let pos = (k + 1) * len / (edits + 1);
        for b in &mut v2[pos..pos + 24] {
            *b ^= 0x5A;
        }
    }
    (v1, v2)
}

#[test]
fn delta_coding_against_earlier_version_and_reference() {
    let dir = std::env::temp_dir().join(format!("tsaur-test-delta-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x0bad_5eed_1234_9876u64;
    let (v1, v2) = versioned_pair(&mut seed, 1 << 20, 40);
    let mut v3 = v2.clone();
    for k in 0..30 {
        let pos = 7_000 + k * 31_000;
        v3[pos] ^= 0x33;
    }
    std::fs::write(src.join("report-A.bin"), &v1).unwrap();
    std::fs::write(src.join("report-A.v2.bin"), &v2).unwrap();
    std::fs::write(src.join("report-A.v3.bin"), &v3).unwrap();

    // 1. intra-archive: the second file is delta-coded against the first
    let plain = dir.join("nodelta.tsr");
    let with = dir.join("delta.tsr");
    let r0 = pack(std::slice::from_ref(&src), &plain, PackOptions { delta: false, ..Default::default() }).unwrap();
    let r1 = pack(std::slice::from_ref(&src), &with, PackOptions::default()).unwrap();
    assert!(r1.archive_bytes < r0.archive_bytes * 6 / 10, "delta must shrink the new version substantially: {} vs {}", r1.archive_bytes, r0.archive_bytes);
    assert!(r1.codec_hist.contains_key("zstd+delta"));
    let mut r = Reader::open(&with, None).unwrap();
    assert!(r.index.blobs.iter().any(|b| b.codec == tsaur_core::codec::ZSTD_DELTA && !b.d.is_empty()));
    let e2 = r.find_entry("report-A.v2.bin").unwrap();
    assert_eq!(r.entry_bytes(e2).unwrap(), v2);
    let e3 = r.find_entry("report-A.v3.bin").unwrap();
    assert_eq!(r.entry_bytes(e3).unwrap(), v3, "a third version must decode (dictionaries never chain)");
    assert_eq!(r.read_range(e2, 700_000, 5000).unwrap(), v2[700_000..705_000]);
    let ex = dir.join("out");
    r.extract(&ex, None, false).unwrap();
    assert_eq!(std::fs::read(ex.join("report-A.v2.bin")).unwrap(), v2);
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
    // with a block large enough for every version, the codec window already covers the earlier
    // versions: no delta and no block split
    let solid = dir.join("solid.tsr");
    let rs = pack(std::slice::from_ref(&src), &solid, PackOptions { block_size: 8 << 20, ..Default::default() }).unwrap();
    assert_eq!(rs.blobs, 1);
    assert!(!rs.codec_hist.contains_key("zstd+delta"));
    assert!(rs.archive_bytes < r0.archive_bytes * 6 / 10, "solid: {} vs {}", rs.archive_bytes, r0.archive_bytes);

    // 2. by reference: v2 alone, delta-coded against v1 stored in another archive
    let base_dir = dir.join("base");
    let new_dir = dir.join("new");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(base_dir.join("report.bin"), &v1).unwrap();
    std::fs::write(new_dir.join("report.bin"), &v2).unwrap();
    let base = dir.join("base.tsr");
    pack(std::slice::from_ref(&base_dir), &base, PackOptions::default()).unwrap();
    let full = dir.join("full.tsr");
    let inc = dir.join("inc.tsr");
    pack(std::slice::from_ref(&new_dir), &full, PackOptions::default()).unwrap();
    let ri = pack(std::slice::from_ref(&new_dir), &inc, PackOptions { references: vec![base.clone()], ..Default::default() }).unwrap();
    assert!(ri.archive_bytes < std::fs::metadata(&full).unwrap().len() / 10, "incremental archive must be tiny: {} bytes", ri.archive_bytes);
    assert!(ri.referenced_chunks > 0);
    let mut r = Reader::open_with_refs(&inc, None, std::slice::from_ref(&base)).unwrap();
    assert_eq!(r.entry_bytes(0).unwrap(), v2);
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
    // a renamed new version still finds its base by name similarity inside the reference
    let ren_dir = dir.join("renamed");
    std::fs::create_dir_all(&ren_dir).unwrap();
    std::fs::write(ren_dir.join("report.v2.bin"), &v2).unwrap();
    let inc2 = dir.join("inc2.tsr");
    let ri2 = pack(std::slice::from_ref(&ren_dir), &inc2, PackOptions { references: vec![base.clone()], ..Default::default() }).unwrap();
    assert!(ri2.archive_bytes < std::fs::metadata(&full).unwrap().len() / 10, "renamed incremental archive must be tiny: {} bytes", ri2.archive_bytes);
    assert!(ri2.codec_hist.contains_key("zstd+delta"));
    let mut r = Reader::open_with_refs(&inc2, None, std::slice::from_ref(&base)).unwrap();
    assert_eq!(r.entry_bytes(0).unwrap(), v2);
    // without the reference the dictionary chunks are missing: a clear error, no panic
    let mut r = Reader::open(&inc, None).unwrap();
    assert!(r.unresolved_external() > 0);
    assert!(matches!(r.entry_bytes(0), Err(tsaur_core::Error::Missing(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unrelated_files_with_similar_names_do_not_fragment_solid_blocks() {
    let dir = std::env::temp_dir().join(format!("tsaur-test-nosplit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x7777_1234_5678_9abcu64;
    let a: Vec<u8> = (0..300_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    let b: Vec<u8> = (0..300_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("report-A.bin"), &a).unwrap();
    std::fs::write(src.join("report-B.bin"), &b).unwrap();
    let out = dir.join("s.tsr");
    let rep = pack(std::slice::from_ref(&src), &out, PackOptions { block_size: 4 << 20, ..Default::default() }).unwrap();
    assert_eq!(rep.blobs, 1, "no shared chunk, so no delta candidate and no block split");
    assert!(!rep.codec_hist.contains_key("zstd+delta"));
    let _ = std::fs::remove_dir_all(&dir);
}
