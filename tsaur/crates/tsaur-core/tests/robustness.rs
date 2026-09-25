//! Robustness campaign with systematically damaged inputs, on the stable toolchain (no external
//! fuzzer). Every parser that consumes bytes from outside the process is fed mutated variants of
//! valid inputs and must return an error or a verified result: never a panic, never a file
//! outside its destination, never unverified bytes presented as content.
//!
//! Targets: the `.tsr` reader (open, list, verify, extract, inspect), `.tsrv` volumes (inspect,
//! join, repair), the volume descriptor (CBOR decode + validate), the transfer request line on a
//! live server, the transfer replies as seen by `fetch`, and the resume files. The default run is
//! short so `cargo test` stays fast; `TSAUR_ROBUSTNESS_SECONDS=<n>` runs each target for about
//! `n` seconds and prints the iteration counts (that is the campaign reported in
//! `docs/review/RC1-VERIFICATION-REPORT.md`). Any input that panics is saved under
//! `target/robustness-findings/` so that it can become a regression test.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsaur_core::transfer::{self, FetchOptions, Server, ServerLimits, Want};
use tsaur_core::volumes::{self, SplitOptions, VolumeSet};
use tsaur_core::{pack, PackOptions, Reader};

fn budget() -> Duration {
    std::env::var("TSAUR_ROBUSTNESS_SECONDS").ok().and_then(|s| s.parse::<u64>().ok()).map(Duration::from_secs).unwrap_or(Duration::from_millis(1500))
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// One random mutation of `data`: bit flip, byte set, range zeroed, range deleted, bytes inserted,
/// range duplicated, truncation, or a burst of several of these.
fn mutate(rng: &mut Rng, data: &[u8]) -> Vec<u8> {
    let mut v = data.to_vec();
    let rounds = 1 + rng.below(3);
    for _ in 0..rounds {
        if v.is_empty() {
            v.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(v.len());
        match rng.below(8) {
            0 => v[at] ^= 1 << rng.below(8),
            1 => v[at] = rng.next() as u8,
            2 => {
                let end = (at + 1 + rng.below(64)).min(v.len());
                v[at..end].iter_mut().for_each(|b| *b = 0);
            }
            3 => {
                let end = (at + 1 + rng.below(64)).min(v.len());
                v.drain(at..end);
            }
            4 => {
                let n = 1 + rng.below(32);
                let ins: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                v.splice(at..at, ins);
            }
            5 => {
                let end = (at + 1 + rng.below(64)).min(v.len());
                let dup = v[at..end].to_vec();
                v.splice(end..end, dup);
            }
            6 => v.truncate(at.max(1)),
            _ => {
                // structured field damage: overwrite a little-endian u32/u64 with an extreme value
                let val: u64 = [0, 1, u32::MAX as u64, u64::MAX, 0x8000_0000, data.len() as u64][rng.below(6)];
                let width = if rng.below(2) == 0 { 4 } else { 8 };
                let end = (at + width).min(v.len());
                v[at..end].copy_from_slice(&val.to_le_bytes()[..end - at]);
            }
        }
    }
    v
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tsaur-robust-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn findings_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("robustness-findings");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs `f` on mutated inputs until the budget ends; a panic saves the input and fails the test.
fn campaign(name: &str, seed: u64, base: &[Vec<u8>], mut f: impl FnMut(&[u8]) -> &'static str) -> usize {
    let mut rng = Rng(seed | 1);
    let start = Instant::now();
    let mut iterations = 0usize;
    let mut outcomes = std::collections::BTreeMap::<&'static str, usize>::new();
    while start.elapsed() < budget() {
        let src = &base[rng.below(base.len())];
        let input = mutate(&mut rng, src);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&input)));
        match result {
            Ok(outcome) => *outcomes.entry(outcome).or_default() += 1,
            Err(_) => {
                let path = findings_dir().join(format!("{name}-{iterations}.bin"));
                std::fs::write(&path, &input).unwrap();
                panic!("{name}: iteration {iterations} panicked; input saved to {}", path.display());
            }
        }
        iterations += 1;
    }
    println!("robustness {name}: {iterations} iterations in {:.1} s, outcomes {outcomes:?}", start.elapsed().as_secs_f64());
    assert!(iterations > 0);
    iterations
}

struct Lab {
    dir: PathBuf,
    inputs: Vec<(String, Vec<u8>)>,
    archive: Vec<u8>,
    volumes: Vec<Vec<u8>>,
    volume_paths: Vec<PathBuf>,
    set: VolumeSet,
    descriptor: Vec<u8>,
    set_id: [u8; 32],
    descriptor_b3: [u8; 32],
}

fn lab(tag: &str) -> Lab {
    let dir = temp(tag);
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ tag.len() as u64);
    let text: String = (0..3000).map(|i| ["chunk ", "hash ", "verify ", "restore ", "piece\n"][(rng.next() % 5) as usize].to_string() + if i % 7 == 0 { "\n" } else { "" }).collect();
    let noise: Vec<u8> = (0..150_000).map(|_| (rng.next() >> 56) as u8).collect();
    let inputs = vec![("a.txt".to_string(), text.clone().into_bytes()), ("noise.bin".to_string(), noise), ("sub/a-v2.txt".to_string(), (text + "tail\n").into_bytes())];
    for (rel, bytes) in &inputs {
        std::fs::write(src.join(rel), bytes).unwrap();
    }
    let archive_path = dir.join("lab.tsr");
    pack(std::slice::from_ref(&src), &archive_path, PackOptions::default()).unwrap();
    let vol_dir = dir.join("vols");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&archive_path, &SplitOptions { data: 2, parity: 1, piece_size: Some(65536), outputs: vec![vol_dir.clone()] }).unwrap();
    let status = volumes::inspect(&[vol_dir], false).unwrap().remove(0);
    let descriptor = tsaur_core::format::cbor_encode(&status.set).unwrap();
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    let mut descriptor_b3 = [0u8; 32];
    descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    let volume_paths: Vec<PathBuf> = rep.volumes.iter().map(|v| v.path.clone()).collect();
    Lab {
        dir,
        inputs,
        archive: std::fs::read(&archive_path).unwrap(),
        volumes: volume_paths.iter().map(|p| std::fs::read(p).unwrap()).collect(),
        volume_paths,
        set: status.set,
        descriptor,
        set_id,
        descriptor_b3,
    }
}

#[test]
fn archive_reader_survives_damaged_archives_and_never_returns_wrong_bytes() {
    let l = lab("reader");
    let work = temp("reader-work");
    let path = work.join("m.tsr");
    let out = work.join("out");
    let expected = l.inputs.clone();
    campaign("archive-reader", 1, std::slice::from_ref(&l.archive), |input| {
        std::fs::write(&path, input).unwrap();
        let _ = std::fs::remove_dir_all(&out);
        let _ = tsaur_core::inspect(&path);
        let mut r = match Reader::open(&path, None) {
            Ok(r) => r,
            Err(_) => return "refused at open",
        };
        let entries: Vec<String> = r.entries().iter().map(|e| e.path.clone()).collect();
        let report = match r.verify(None) {
            Ok(v) => v,
            Err(_) => return "verify errored",
        };
        match r.extract(&out, None, false) {
            Ok(written) => {
                // whatever was written must be exactly what the untouched inputs contained
                for w in written {
                    let rel = w.strip_prefix(&out).unwrap().to_string_lossy().replace('\\', "/");
                    let orig = expected.iter().find(|(p, _)| *p == rel).map(|(_, b)| b.clone()).unwrap_or_default();
                    assert_eq!(std::fs::read(&w).unwrap(), orig, "extracted {rel} differs from the input although it verified");
                    assert!(entries.contains(&rel));
                }
                if report.entries_bad.is_empty() {
                    "opened, verified, restored bit-exact"
                } else {
                    "opened with bad entries, rest restored"
                }
            }
            Err(_) => "extract errored",
        }
    });
    let _ = std::fs::remove_dir_all(&l.dir);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn volume_tools_survive_damaged_volumes_and_never_join_wrong_bytes() {
    let l = lab("volumes");
    let work = temp("volumes-work");
    let vdir = work.join("v");
    let joined = work.join("joined.tsr");
    let repaired = work.join("repaired");
    let names: Vec<String> = l.volume_paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
    let archive = l.archive.clone();
    let mut which = Rng(77);
    campaign("volumes", 2, &l.volumes, |input| {
        let _ = std::fs::remove_dir_all(&vdir);
        std::fs::create_dir_all(&vdir).unwrap();
        // one damaged volume among the intact ones (any two of three suffice, so the damaged one
        // may or may not matter)
        let damaged = which.below(3);
        for (i, name) in names.iter().enumerate() {
            let bytes = if i == damaged { input } else { &l.volumes[i] };
            std::fs::write(vdir.join(name), bytes).unwrap();
        }
        let status = match volumes::inspect(std::slice::from_ref(&vdir), true) {
            Ok(s) => s,
            Err(_) => return "inspect errored",
        };
        let _ = std::fs::remove_file(&joined);
        let join = volumes::join(std::slice::from_ref(&vdir), &joined);
        if join.is_ok() {
            assert_eq!(std::fs::read(&joined).unwrap(), archive, "join succeeded with wrong bytes");
        } else {
            assert!(!joined.exists(), "a failed join must not leave an output file");
        }
        let _ = std::fs::remove_dir_all(&repaired);
        std::fs::create_dir_all(&repaired).unwrap();
        let _ = volumes::repair(std::slice::from_ref(&vdir), Some(&repaired));
        match (status.len(), join.is_ok()) {
            (0, _) => "no set recognised",
            (_, true) => "joined bit-exact",
            (_, false) => "set seen, join refused",
        }
    });
    let _ = std::fs::remove_dir_all(&l.dir);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn manifest_and_chunk_index_decoding_never_panic() {
    // the reader authenticates every section before decoding it, so damage to the bytes on disk
    // is caught by the hashes; this target feeds the decoders directly with damaged CBOR, the
    // shape an attacker would need a valid hash for
    let l = lab("manifest");
    let path = l.dir.join("lab.tsr");
    let r = Reader::open(&path, None).unwrap();
    let manifest = tsaur_core::format::cbor_encode(&r.manifest).unwrap();
    let index = tsaur_core::format::cbor_encode(&r.index).unwrap();
    campaign("manifest-and-index", 8, &[manifest, index], |input| {
        let m = tsaur_core::format::cbor_decode::<tsaur_core::manifest::Manifest>(input);
        let i = tsaur_core::format::cbor_decode::<tsaur_core::manifest::ChunkIndex>(input);
        match (m.is_ok(), i.is_ok()) {
            (true, _) => {
                let m = m.unwrap();
                for e in &m.entries {
                    let _ = tsaur_core::paths::normalize(&e.path);
                }
                "manifest decoded"
            }
            (_, true) => "chunk index decoded",
            _ => "not decodable",
        }
    });
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn volume_descriptor_decoding_and_validation_never_panic() {
    let l = lab("descriptor");
    campaign("descriptor", 3, std::slice::from_ref(&l.descriptor), |input| match tsaur_core::format::cbor_decode::<VolumeSet>(input) {
        Err(_) => "not cbor",
        Ok(set) => match set.validate() {
            Err(_) => "decoded, invalid",
            Ok(()) => {
                let _ = set.total();
                let _ = set.volume_name(0);
                let _ = set.payload_len(0);
                "decoded, valid"
            }
        },
    });
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn server_survives_damaged_request_lines_and_still_serves_afterwards() {
    let l = lab("requests");
    // The parser is the target, not the per-address request rate (covered by
    // tests/transfer_limits.rs). The campaign is a tight sequential loop from one address; on a
    // fast runner it exceeds the default 200 requests per second, the token bucket empties and
    // the check after the campaign is refused with 429. So this server admits any rate, and the
    // test fails if the limiter ever fires anyway.
    let limits = ServerLimits { max_requests_per_second: 1_000_000, ..ServerLimits::default() };
    let server = Server::bind_with(&l.volume_paths, "127.0.0.1:0", limits).unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let mut rate_limited = 0usize;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    let id = hex::encode(l.set_id);
    let valid: Vec<Vec<u8>> = vec![
        b"TSXP/1 SETS\n".to_vec(),
        format!("TSXP/1 DESCRIPTOR {id}\n").into_bytes(),
        format!("TSXP/1 HAVE {id}\n").into_bytes(),
        format!("TSXP/1 PIECE {id} 0 0\n").into_bytes(),
        format!("TSXP/1 PIECE {id} 2 1\n").into_bytes(),
    ];
    campaign("request-line", 4, &valid, |input| {
        // the parser is the target, not the read timeout (covered by tests/transfer_limits.rs):
        // a mutated line always ends with a newline so that every iteration gets an answer
        let mut line = input.to_vec();
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = s.write_all(&line);
        let mut status = String::new();
        let _ = BufReader::new(&mut s).read_line(&mut status);
        if status.starts_with("OK ") {
            "served"
        } else if status.starts_with("ERR 429 ") {
            rate_limited += 1;
            "rate limited"
        } else if status.starts_with("ERR ") {
            "refused"
        } else {
            "closed"
        }
    });
    assert_eq!(rate_limited, 0, "the campaign must exercise the parser, never the request-rate limiter");
    assert_eq!(transfer::list_peer(&addr, None, None).unwrap().len(), 1, "the server still answers after the campaign");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

/// A peer that answers every request with one fixed byte string.
fn raw_peer(reply: Arc<std::sync::Mutex<Vec<u8>>>) -> (String, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
                let bytes = reply.lock().unwrap().clone();
                let _ = s.write_all(&bytes);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    });
    (addr, stop)
}

#[test]
fn fetch_survives_damaged_replies_and_never_writes_unverified_volumes() {
    let l = lab("replies");
    let reply = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (addr, stop) = raw_peer(reply.clone());
    let work = temp("replies-work");
    let out = work.join("out");
    let descriptor_reply = {
        let mut v = format!("OK {}\n", l.descriptor.len()).into_bytes();
        v.extend_from_slice(&l.descriptor);
        v
    };
    let have_reply = {
        let have = tsaur_core::format::cbor_encode(&vec![0u16, 1, 2]).unwrap();
        let mut v = format!("OK {}\n", have.len()).into_bytes();
        v.extend_from_slice(&have);
        v
    };
    let piece_reply = {
        let ps = l.set.piece_size as usize;
        let mut v = format!("OK {ps}\n").into_bytes();
        v.extend_from_slice(&l.volumes[0][64..64 + ps]);
        v
    };
    let base = vec![descriptor_reply, have_reply, piece_reply, b"ERR 404 unknown set\n".to_vec()];
    let opts =
        FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers: vec![addr.clone()], out_dir: out.clone(), local: Vec::new(), want: Want::Needed, max_pieces: Some(2), tls: None };
    campaign("fetch-replies", 5, &base, |input| {
        *reply.lock().unwrap() = input.to_vec();
        let _ = std::fs::remove_dir_all(&out);
        let res = transfer::fetch(&opts);
        // a volume file may exist only when every one of its pieces verified, which a fixed reply
        // can never achieve for a whole volume (pieces differ), so no `.tsrv` may appear
        if out.is_dir() {
            for e in std::fs::read_dir(&out).unwrap() {
                let p = e.unwrap().path();
                assert!(!p.to_string_lossy().ends_with(".tsrv"), "unverified volume written: {}", p.display());
            }
        }
        match res {
            Ok(r) if r.pieces_received > 0 => "some pieces accepted (verified)",
            Ok(_) => "nothing accepted",
            Err(_) => "fetch errored",
        }
    });
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn resume_files_can_be_arbitrarily_damaged_without_ever_yielding_wrong_volumes() {
    let l = lab("resume");
    let server = Server::bind(&l.volume_paths, "127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    let work = temp("resume-work");
    let out = work.join("out");
    // produce genuine partial files once
    let first = transfer::fetch(&FetchOptions {
        set_id: l.set_id,
        descriptor_b3: Some(l.descriptor_b3),
        peers: vec![addr.clone()],
        out_dir: out.clone(),
        local: Vec::new(),
        want: Want::Needed,
        max_pieces: Some(1),
        tls: None,
    })
    .unwrap();
    assert!(first.stopped_early);
    let partial: Vec<PathBuf> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).filter(|p| p.to_string_lossy().ends_with(".partial")).collect();
    let map: Vec<PathBuf> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).filter(|p| p.to_string_lossy().ends_with(".partial.map")).collect();
    assert_eq!((partial.len(), map.len()), (1, 1));
    let partial_bytes = std::fs::read(&partial[0]).unwrap();
    let map_bytes = std::fs::read(&map[0]).unwrap();
    let archive = l.archive.clone();
    let mut which = Rng(5);
    let opts =
        FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers: vec![addr.clone()], out_dir: out.clone(), local: Vec::new(), want: Want::Needed, max_pieces: None, tls: None };
    campaign("resume-files", 6, &[partial_bytes.clone(), map_bytes.clone()], |input| {
        // restore the genuine pair, then damage one of the two files
        for f in std::fs::read_dir(&out).unwrap() {
            let _ = std::fs::remove_file(f.unwrap().path());
        }
        std::fs::write(&partial[0], &partial_bytes).unwrap();
        std::fs::write(&map[0], &map_bytes).unwrap();
        if which.below(2) == 0 {
            std::fs::write(&partial[0], input).unwrap();
        } else {
            std::fs::write(&map[0], input).unwrap();
        }
        let res = transfer::fetch(&opts);
        let joined = work.join("j.tsr");
        let _ = std::fs::remove_file(&joined);
        match res {
            Ok(r) if r.reconstructible => {
                volumes::join(std::slice::from_ref(&out), &joined).unwrap();
                assert_eq!(std::fs::read(&joined).unwrap(), archive, "resumed volumes joined to wrong bytes");
                "resumed and joined bit-exact"
            }
            Ok(_) => "fetch incomplete",
            Err(_) => "fetch errored",
        }
    });
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn extraction_destination_is_never_escaped_by_any_damage() {
    // paths are re-validated on the way out: even a damaged manifest that still decodes cannot
    // point outside the destination; this checks the invariant on the damaged archives directly
    let l = lab("escape");
    let work = temp("escape-work");
    let path = work.join("m.tsr");
    let out = work.join("out");
    let canary = work.join("canary.txt");
    campaign("extract-destination", 7, std::slice::from_ref(&l.archive), |input| {
        std::fs::write(&path, input).unwrap();
        let _ = std::fs::remove_dir_all(&out);
        std::fs::write(&canary, b"untouched").unwrap();
        let Ok(mut r) = Reader::open(&path, None) else { return "refused at open" };
        let _ = r.extract(&out, None, false);
        assert_eq!(std::fs::read(&canary).unwrap(), b"untouched");
        for e in walk(&work) {
            assert!(e.starts_with(&out) || e == path || e == canary, "file outside the destination: {}", e.display());
        }
        "checked"
    });
    let _ = std::fs::remove_dir_all(&l.dir);
    let _ = std::fs::remove_dir_all(&work);
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}
