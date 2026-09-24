//! Offline volume sets: split across directories, lose volumes, damage pieces and headers,
//! rebuild the archive bit-exact, recreate volumes, never panic on corrupt inputs.

use std::path::{Path, PathBuf};
use tsaur_core::volumes::{self, SplitOptions};
use tsaur_core::{pack, Error, PackOptions, Reader};

fn xorshift(seed: &mut u64) -> u64 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    x
}

struct Lab {
    dir: PathBuf,
    archive: PathBuf,
    archive_bytes: Vec<u8>,
    outs: Vec<PathBuf>,
}

/// An archive of ~1.5 MB (mostly incompressible, so the volumes are not trivially small) plus
/// `locations` empty output directories.
fn lab(tag: &str, locations: usize) -> Lab {
    let dir = std::env::temp_dir().join(format!("tsaur-vol-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x5eed_0000_c0ff_ee00u64 ^ tag.len() as u64;
    let big: Vec<u8> = (0..1_400_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
    let text: String = (0..4000).map(|i| format!("line {i}: volumes spread an archive over devices\n")).collect();
    std::fs::write(src.join("notes.txt"), text).unwrap();
    let archive = dir.join("set.tsr");
    pack(std::slice::from_ref(&src), &archive, PackOptions::default()).unwrap();
    let archive_bytes = std::fs::read(&archive).unwrap();
    assert!(archive_bytes.len() > 1_300_000);
    let outs: Vec<PathBuf> = (0..locations).map(|i| dir.join(format!("drive{i}"))).collect();
    for o in &outs {
        std::fs::create_dir_all(o).unwrap();
    }
    Lab { dir, archive, archive_bytes, outs }
}

fn split(l: &Lab, data: u16, parity: u16, piece_kib: u32) -> volumes::SplitReport {
    volumes::split(&l.archive, &SplitOptions { data, parity, piece_size: Some(piece_kib * 1024), outputs: l.outs.clone() }).unwrap()
}

fn assert_joined_identical(l: &Lab, inputs: &[PathBuf], out: &Path) -> volumes::JoinReport {
    let rep = volumes::join(inputs, out).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), l.archive_bytes, "joined archive must be bit-identical");
    let mut r = Reader::open(out, None).unwrap();
    let v = r.verify(None).unwrap();
    assert!(v.entries_bad.is_empty());
    rep
}

#[test]
fn any_n_of_n_plus_m_volumes_rebuild_the_archive() {
    let l = lab("anyn", 6);
    let rep = split(&l, 4, 2, 64);
    assert_eq!(rep.volumes.len(), 6);
    assert_eq!(rep.pieces as usize, l.archive_bytes.len().div_ceil(65536));
    assert!(!rep.placement_warning, "one location per volume: no warning");
    let all: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    // every pair of volumes may be missing (15 combinations)
    let mut combos = 0;
    for a in 0..6 {
        for b in a + 1..6 {
            let inputs: Vec<PathBuf> = all.iter().enumerate().filter(|(i, _)| *i != a && *i != b).map(|(_, p)| p.clone()).collect();
            let out = l.dir.join(format!("join-{a}-{b}.tsr"));
            let jr = assert_joined_identical(&l, &inputs, &out);
            assert_eq!(jr.volumes_missing, vec![a, b]);
            let expect_rebuilt = if a < 4 && b < 4 {
                2 * rep.stripes
            } else if a < 4 || b < 4 {
                rep.stripes
            } else {
                0
            };
            assert!(jr.pieces_rebuilt <= expect_rebuilt, "{a},{b}: rebuilt {} > {expect_rebuilt}", jr.pieces_rebuilt);
            combos += 1;
        }
    }
    assert_eq!(combos, 15);
    // the whole set as directories, nothing missing
    let out = l.dir.join("join-all.tsr");
    let jr = assert_joined_identical(&l, &l.outs, &out);
    assert_eq!(jr.pieces_rebuilt, 0);
    // three volumes missing: refused before anything is written
    let out = l.dir.join("join-fail.tsr");
    let inputs: Vec<PathBuf> = all[3..].to_vec();
    let err = volumes::join(&inputs, &out).unwrap_err();
    assert!(matches!(err, Error::Missing(_)), "{err}");
    assert!(!out.exists() && !l.dir.join("join-fail.tsr.tsr-partial").exists());
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn damaged_pieces_are_erasures_and_verification_reports_them() {
    let l = lab("damage", 6);
    let rep = split(&l, 4, 2, 64);
    let all: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    let ps = 65536usize;
    // two damaged pieces in the same stripe on two volumes: within parity
    for vi in [0usize, 3] {
        let mut bytes = std::fs::read(&all[vi]).unwrap();
        let off = 64 + 2 * ps + 100; // stripe 2
        bytes[off] ^= 0xFF;
        std::fs::write(&all[vi], &bytes).unwrap();
    }
    let status = &volumes::inspect(&all, true).unwrap()[0];
    assert_eq!(status.volumes[0].as_ref().unwrap().bad_pieces, vec![2]);
    assert_eq!(status.volumes[3].as_ref().unwrap().bad_pieces, vec![2]);
    assert!(status.reconstructible);
    let out = l.dir.join("join-2bad.tsr");
    let jr = assert_joined_identical(&l, &all, &out);
    assert_eq!(jr.pieces_rebuilt, 2);
    // a third damaged piece in the same stripe: that stripe is beyond parity
    let mut bytes = std::fs::read(&all[5]).unwrap();
    bytes[64 + 2 * ps + 7] ^= 0x01;
    std::fs::write(&all[5], &bytes).unwrap();
    let status = &volumes::inspect(&all, true).unwrap()[0];
    assert!(!status.reconstructible);
    assert_eq!(status.stripes_short, 1);
    let out = l.dir.join("join-3bad.tsr");
    let err = volumes::join(&all, &out).unwrap_err();
    assert!(matches!(err, Error::Missing(_)), "{err}");
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn damaged_header_or_trailer_does_not_disable_a_volume() {
    let l = lab("ends", 6);
    let rep = split(&l, 4, 2, 64);
    let all: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    let originals: Vec<Vec<u8>> = all.iter().map(|p| std::fs::read(p).unwrap()).collect();
    // header of v3 zeroed: identified by its trailer
    let mut b = std::fs::read(&all[2]).unwrap();
    b[..64].fill(0);
    std::fs::write(&all[2], &b).unwrap();
    // trailer of v4 zeroed: identified by its header, descriptor taken from a sibling
    let mut b = std::fs::read(&all[3]).unwrap();
    let n = b.len();
    b[n - 56..].fill(0);
    std::fs::write(&all[3], &b).unwrap();
    let status = &volumes::inspect(&all, true).unwrap()[0];
    let v3 = status.volumes[2].as_ref().unwrap();
    let v4 = status.volumes[3].as_ref().unwrap();
    assert!(!v3.header_ok && v3.descriptor_ok && v3.usable());
    assert!(v4.header_ok && !v4.descriptor_ok && v4.usable());
    assert!(status.missing.is_empty());
    // both ends damaged on v5: it cannot be placed, the other five still rebuild everything
    let mut b = std::fs::read(&all[4]).unwrap();
    let n = b.len();
    b[..64].fill(0);
    b[n - 56..].fill(0);
    std::fs::write(&all[4], &b).unwrap();
    let status = &volumes::inspect(&all, true).unwrap()[0];
    assert_eq!(status.missing, vec![4]);
    assert!(status.extra.iter().any(|(p, _)| p == &all[4]));
    let out = l.dir.join("join-ends.tsr");
    let jr = assert_joined_identical(&l, &all, &out);
    assert_eq!(jr.volumes_missing, vec![4]);
    // repair rewrites the two damaged files in place and recreates the unplaceable one next to
    // the first intact volume (the unplaceable file cannot be identified, so it is not touched)
    let rr = volumes::repair(&all, None).unwrap();
    let mut fixed: Vec<usize> = rr.rebuilt.iter().map(|v| v.index).collect();
    fixed.sort_unstable();
    assert_eq!(fixed, vec![2, 3, 4]);
    for v in &rr.rebuilt {
        assert_eq!(std::fs::read(&v.path).unwrap(), originals[v.index], "volume {} must be byte-identical after repair", v.index + 1);
    }
    assert_eq!(rr.rebuilt.iter().find(|v| v.index == 4).unwrap().path.parent().unwrap(), all[0].parent().unwrap());
    std::fs::remove_file(&all[4]).unwrap(); // the unidentifiable leftover
    let mut all: Vec<PathBuf> = all[..4].to_vec();
    all.push(rr.rebuilt.iter().find(|v| v.index == 4).unwrap().path.clone());
    all.push(rep.volumes[5].path.clone());
    let status = &volumes::inspect(&all, true).unwrap()[0];
    assert!(status.missing.is_empty() && status.volumes.iter().flatten().all(|v| v.usable() && v.header_ok && v.descriptor_ok));
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn repair_recreates_lost_volumes_byte_identical() {
    let l = lab("repair", 3);
    let rep = split(&l, 4, 2, 64);
    assert!(!rep.placement_warning, "6 volumes over 3 locations: losing one location loses 2 volumes = parity, which is still recoverable; the warning fires only above parity");
    assert_eq!(rep.max_per_location, 2);
    let all: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    let originals: Vec<Vec<u8>> = all.iter().map(|p| std::fs::read(p).unwrap()).collect();
    // lose "drive1": volumes 2 and 5 (round-robin placement)
    let lost: Vec<usize> = rep.volumes.iter().filter(|v| v.path.starts_with(&l.outs[1])).map(|v| v.index).collect();
    assert_eq!(lost, vec![1, 4]);
    std::fs::remove_dir_all(&l.outs[1]).unwrap();
    let remaining = vec![l.outs[0].clone(), l.outs[2].clone()];
    let status = &volumes::inspect(&remaining, true).unwrap()[0];
    assert_eq!(status.missing, vec![1, 4]);
    assert!(status.reconstructible);
    let new_drive = l.dir.join("drive-new");
    std::fs::create_dir_all(&new_drive).unwrap();
    let rr = volumes::repair(&remaining, Some(&new_drive)).unwrap();
    assert_eq!(rr.rebuilt.len(), 2);
    for v in &rr.rebuilt {
        assert_eq!(std::fs::read(&v.path).unwrap(), originals[v.index], "recreated volume {} must be byte-identical", v.index + 1);
    }
    let complete = vec![l.outs[0].clone(), l.outs[2].clone(), new_drive.clone()];
    let status = &volumes::inspect(&complete, true).unwrap()[0];
    assert!(status.missing.is_empty() && status.volumes.iter().flatten().all(|v| v.usable()));
    // nothing to repair now
    assert!(volumes::repair(&complete, None).unwrap().rebuilt.is_empty());
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn split_without_parity_is_plain_striping() {
    let l = lab("noparity", 3);
    let rep = split(&l, 3, 0, 128);
    assert_eq!(rep.volumes.len(), 3);
    assert!(rep.placement_warning, "with no parity, any loss is fatal");
    let out = l.dir.join("join.tsr");
    assert_joined_identical(&l, &l.outs, &out);
    std::fs::remove_file(&rep.volumes[1].path).unwrap();
    let err = volumes::join(&l.outs, &l.dir.join("join2.tsr")).unwrap_err();
    assert!(matches!(err, Error::Missing(_)), "{err}");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn inspect_groups_sets_and_reports_strangers() {
    let l = lab("groups", 1);
    let rep1 = split(&l, 2, 1, 64);
    // a second archive (different content) split into the same directory
    let other = l.dir.join("other.tsr");
    let src2 = l.dir.join("src2");
    std::fs::create_dir_all(&src2).unwrap();
    std::fs::write(src2.join("a.txt"), "another archive entirely\n".repeat(5000)).unwrap();
    pack(std::slice::from_ref(&src2), &other, PackOptions::default()).unwrap();
    let rep2 = volumes::split(&other, &SplitOptions { data: 2, parity: 1, piece_size: Some(65536), outputs: l.outs.clone() }).unwrap();
    assert_ne!(rep1.set_id, rep2.set_id);
    // a stranger with the volume extension
    std::fs::write(l.outs[0].join("junk.v01.tsrv"), b"not a volume at all").unwrap();
    let sets = volumes::inspect(&l.outs, true).unwrap();
    assert_eq!(sets.len(), 2);
    for s in &sets {
        assert!(s.reconstructible && s.missing.is_empty());
        assert!(s.extra.iter().any(|(p, _)| p.ends_with("junk.v01.tsrv")));
    }
    // joining a directory that holds two sets is refused
    let err = volumes::join(&l.outs, &l.dir.join("mixed.tsr")).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    // but the volumes of one set, given explicitly, join
    let inputs: Vec<PathBuf> = rep2.volumes.iter().map(|v| v.path.clone()).collect();
    let out = l.dir.join("other-joined.tsr");
    volumes::join(&inputs, &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&other).unwrap());
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn random_volume_corruption_never_panics_and_never_yields_wrong_bytes() {
    let l = lab("fuzz", 5);
    let rep = split(&l, 3, 2, 64);
    let all: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    let originals: Vec<Vec<u8>> = all.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let mut seed = 0xfeed_face_1234_5678u64;
    let mut ok = 0;
    let mut refused = 0;
    for round in 0..60u32 {
        // restore, then damage one or two volumes
        for (p, o) in all.iter().zip(&originals) {
            std::fs::write(p, o).unwrap();
        }
        let victims = 1 + (xorshift(&mut seed) % 2) as usize;
        for _ in 0..victims {
            let vi = (xorshift(&mut seed) as usize) % all.len();
            let mut b = originals[vi].clone();
            match xorshift(&mut seed) % 4 {
                0 => {
                    let n = (xorshift(&mut seed) as usize) % b.len();
                    b.truncate(n);
                }
                1 => {
                    for _ in 0..8 {
                        let i = (xorshift(&mut seed) as usize) % b.len();
                        b[i] ^= 1 << (xorshift(&mut seed) % 8);
                    }
                }
                2 => {
                    let i = (xorshift(&mut seed) as usize) % b.len();
                    let n = ((xorshift(&mut seed) as usize) % 5000).min(b.len() - i);
                    for x in &mut b[i..i + n] {
                        *x = (xorshift(&mut seed) >> 40) as u8;
                    }
                }
                _ => b.clear(),
            }
            std::fs::write(&all[vi], &b).unwrap();
        }
        let out = l.dir.join(format!("fuzz-{round}.tsr"));
        match volumes::join(&all, &out) {
            Ok(_) => {
                assert_eq!(std::fs::read(&out).unwrap(), l.archive_bytes, "round {round}: a successful join must be exact");
                ok += 1;
            }
            Err(_) => {
                assert!(!out.exists(), "round {round}: no output on failure");
                refused += 1;
            }
        }
        let _ = volumes::inspect(&all, true);
    }
    assert!(ok > 0 && ok + refused == 60, "ok {ok}, refused {refused}");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn geometry_and_option_errors_are_clear() {
    let l = lab("errors", 1);
    let bad = |data: u16, parity: u16, piece: u32| volumes::split(&l.archive, &SplitOptions { data, parity, piece_size: Some(piece), outputs: l.outs.clone() }).unwrap_err();
    assert!(matches!(bad(0, 1, 65536), Error::Invalid(_)));
    assert!(matches!(bad(200, 100, 65536), Error::Invalid(_)));
    assert!(matches!(bad(2, 1, 1024), Error::Invalid(_)));
    let err = volumes::split(&l.archive, &SplitOptions { data: 2, parity: 1, piece_size: Some(65536), outputs: vec![l.dir.join("nope")] }).unwrap_err();
    assert!(matches!(err, Error::Missing(_)));
    let err = volumes::inspect(&[l.dir.join("empty-dir-that-does-not-exist")], false).unwrap_err();
    assert!(matches!(err, Error::Io(_)) || matches!(err, Error::Missing(_)), "{err}");
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn adaptive_default_keeps_small_archive_overhead_low() {
    let l = lab("adaptive", 6);
    let rep = volumes::split(&l.archive, &SplitOptions { data: 4, parity: 2, piece_size: None, outputs: l.outs.clone() }).unwrap();
    assert!(rep.piece_size_adaptive);
    assert_eq!(rep.piece_size, volumes::adaptive_piece_size(l.archive_bytes.len() as u64, 4));
    assert_eq!(rep.piece_size, 64 << 10, "1.4 MB over 4 data volumes: the 64 KiB floor");
    let total: u64 = rep.volumes.iter().map(|v| v.bytes).sum();
    let parity = l.archive_bytes.len() as u64 / 2; // 2/4
    assert!(total < l.archive_bytes.len() as u64 + parity + 12 * (64 << 10), "padding must stay within a few pieces: {total}");
    assert!(!rep.descriptor_b3.is_empty());
    let status = &volumes::inspect(&l.outs, true).unwrap()[0];
    assert_eq!(status.descriptor_b3, rep.descriptor_b3, "inspect reports the same descriptor hash as split");
    let out = l.dir.join("joined.tsr");
    assert_joined_identical(&l, &l.outs, &out);
    let _ = std::fs::remove_dir_all(&l.dir);
}
