//! Direct piece exchange between two instances on the loopback interface: fetch what is missing,
//! verify everything, resume after interruption, ignore a lying peer, join with the offline code.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tsaur_core::transfer::{self, FetchOptions, Server, Want};
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
    archive_bytes: Vec<u8>,
    volumes: Vec<PathBuf>,
    set_id: [u8; 32],
    descriptor_b3: [u8; 32],
}

/// An archive split 4+2 with 64 KiB pieces into `dir/src-volumes`.
fn lab(tag: &str) -> Lab {
    let dir = std::env::temp_dir().join(format!("tsaur-xfer-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x1234_abcd_5678_ef00u64 ^ tag.len() as u64;
    let big: Vec<u8> = (0..900_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
    std::fs::write(src.join("notes.txt"), "pieces travel, hashes decide\n".repeat(3000)).unwrap();
    let archive = dir.join("set.tsr");
    pack(std::slice::from_ref(&src), &archive, PackOptions::default()).unwrap();
    let vol_dir = dir.join("src-volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&archive, &SplitOptions { data: 4, parity: 2, piece_size: Some(65536), outputs: vec![vol_dir] }).unwrap();
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    let mut descriptor_b3 = [0u8; 32];
    descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    Lab { dir, archive_bytes: std::fs::read(&archive).unwrap(), volumes: rep.volumes.iter().map(|v| v.path.clone()).collect(), set_id, descriptor_b3 }
}

/// Serve the given volume files on a loopback port; returns (address, stop flag).
fn serve(paths: &[PathBuf]) -> (String, Arc<AtomicBool>) {
    let server = Server::bind(paths, "127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn opts(l: &Lab, peers: Vec<String>, out: &Path, want: Want) -> FetchOptions {
    FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers, out_dir: out.to_path_buf(), local: Vec::new(), want, max_pieces: None, tls: None }
}

fn assert_join(l: &Lab, inputs: &[PathBuf], out: &Path) {
    volumes::join(inputs, out).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), l.archive_bytes);
    let mut r = Reader::open(out, None).unwrap();
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
}

#[test]
fn fetch_needed_volumes_then_join_offline() {
    let l = lab("basic");
    let (addr, stop) = serve(&l.volumes);
    let sets = transfer::list_peer(&addr, None, None).unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].0, hex::encode(l.set_id));
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out, Want::Needed)).unwrap();
    assert_eq!(rep.verified_against, "descriptor");
    assert_eq!(rep.volumes_wanted, vec![0, 1, 2, 3], "the four data volumes suffice");
    assert_eq!(rep.volumes_completed, vec![0, 1, 2, 3]);
    assert_eq!(rep.pieces_rejected, 0);
    assert!(rep.reconstructible && !rep.stopped_early);
    assert!(std::fs::read_dir(&out).unwrap().all(|e| e.unwrap().path().extension().is_some_and(|x| x == "tsrv")), "no partial files left");
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // fetching again transfers nothing
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out, Want::Needed)).unwrap();
    assert_eq!(rep.pieces_received, 0);
    // everything, including parity
    let rep = transfer::fetch(&opts(&l, vec![addr], &out, Want::All)).unwrap();
    assert_eq!(rep.volumes_completed, vec![4, 5]);
    let status = &volumes::inspect(&[out], true).unwrap()[0];
    assert!(status.missing.is_empty() && status.volumes.iter().flatten().all(|v| v.usable() && v.header_ok && v.descriptor_ok));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn interrupted_fetch_resumes_and_finishes() {
    let l = lab("resume");
    let (addr, stop) = serve(&l.volumes);
    let out = l.dir.join("here");
    let mut o = opts(&l, vec![addr.clone()], &out, Want::Needed);
    o.max_pieces = Some(5);
    let rep = transfer::fetch(&o).unwrap();
    assert!(rep.stopped_early);
    assert_eq!(rep.pieces_received, 5);
    assert!(rep.volumes_completed.len() <= 1);
    assert!(!rep.volumes_partial.is_empty());
    assert!(std::fs::read_dir(&out).unwrap().any(|e| e.unwrap().path().to_string_lossy().ends_with(".partial.map")));
    // second run continues from the map: only the remaining pieces travel
    let total_pieces: u64 = (0..4).map(|vi| volumes::inspect(&l.volumes, false).unwrap()[0].set.volume_pieces(vi)).sum();
    let rep2 = transfer::fetch(&opts(&l, vec![addr], &out, Want::Needed)).unwrap();
    assert!(!rep2.stopped_early);
    assert_eq!(rep.pieces_received as u64 + rep2.pieces_received as u64, total_pieces);
    assert_eq!(rep2.volumes_partial, Vec::<usize>::new());
    assert!(rep2.reconstructible);
    assert_join(&l, &[out], &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

/// A peer that answers every request with garbage of the right shape.
fn liar(descriptor: Vec<u8>, piece_size: usize) -> (String, Arc<AtomicBool>) {
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
                let mut line = String::new();
                let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
                let body: Vec<u8> = if line.contains("DESCRIPTOR") {
                    let mut d = descriptor.clone();
                    let n = d.len();
                    d[n / 2] ^= 0x01; // one flipped bit in the descriptor
                    d
                } else if line.contains("HAVE") {
                    vec![0x86, 0, 1, 2, 3, 4, 5] // CBOR: [0,1,2,3,4,5]: claims everything
                } else {
                    vec![0x5Au8; piece_size] // wrong piece bytes
                };
                let _ = s.write_all(format!("OK {}\n", body.len()).as_bytes());
                let _ = s.write_all(&body);
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    });
    (addr, stop)
}

#[test]
fn lying_peer_is_rejected_and_honest_peer_used() {
    let l = lab("liar");
    let status = &volumes::inspect(&l.volumes, false).unwrap()[0];
    let descriptor = tsaur_core::format::cbor_encode(&status.set).unwrap();
    let (bad, stop_bad) = liar(descriptor, 65536);
    let (good, stop_good) = serve(&l.volumes);
    let out = l.dir.join("here");
    // the liar is listed first: its descriptor fails the hash, its pieces fail their hashes
    let rep = transfer::fetch(&opts(&l, vec![bad.clone(), good.clone()], &out, Want::Needed)).unwrap();
    assert!(rep.pieces_rejected > 0, "wrong pieces must be counted as rejected");
    assert_eq!(rep.volumes_completed, vec![0, 1, 2, 3]);
    assert!(rep.peers_used.contains(&good) && !rep.peers_used.contains(&bad));
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // the liar alone: nothing acceptable, nothing written that verifies
    let out2 = l.dir.join("nothing");
    let err = transfer::fetch(&opts(&l, vec![bad], &out2, Want::Needed)).unwrap_err();
    assert!(matches!(err, Error::Missing(_)), "{err}");
    assert!(std::fs::read_dir(&out2).map(|d| d.count()).unwrap_or(0) == 0);
    stop_bad.store(true, Ordering::Relaxed);
    stop_good.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn set_id_only_still_verifies_geometry_and_archive_hash() {
    let l = lab("setid");
    let (addr, stop) = serve(&l.volumes);
    let out = l.dir.join("here");
    let mut o = opts(&l, vec![addr.clone()], &out, Want::Needed);
    o.descriptor_b3 = None;
    let rep = transfer::fetch(&o).unwrap();
    assert_eq!(rep.verified_against, "set id");
    assert_eq!(rep.descriptor_b3, hex::encode(l.descriptor_b3));
    assert_join(&l, &[out], &l.dir.join("joined.tsr"));
    // an unknown set id: the peer has nothing acceptable, nothing is written
    let mut o = opts(&l, vec![addr], &l.dir.join("unknown"), Want::Needed);
    o.set_id = [7u8; 32];
    o.descriptor_b3 = None;
    let err = transfer::fetch(&o).unwrap_err();
    assert!(matches!(err, Error::Missing(_)), "{err}");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn peer_without_some_data_volumes_falls_back_to_parity() {
    let l = lab("parity");
    // the peer holds data volumes 1 and 2 and both parity volumes only
    let partial: Vec<PathBuf> = vec![l.volumes[0].clone(), l.volumes[1].clone(), l.volumes[4].clone(), l.volumes[5].clone()];
    let (addr, stop) = serve(&partial);
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out, Want::Needed)).unwrap();
    assert_eq!(rep.volumes_completed, vec![0, 1, 4, 5]);
    assert!(rep.reconstructible);
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // asking for a volume nobody has is reported as partial, not an error
    let rep = transfer::fetch(&opts(&l, vec![addr], &out, Want::Volumes(vec![2]))).unwrap();
    assert_eq!(rep.volumes_partial, vec![2]);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn garbage_requests_do_not_take_the_server_down() {
    let l = lab("garbage");
    let (addr, stop) = serve(&l.volumes);
    for bad in ["nonsense\n", "TSXP/1 PIECE zz 0 0\n", "TSXP/1 PIECE 00 999 0\n", &"x".repeat(300), "TSXP/1 DESCRIPTOR\n"] {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(bad.as_bytes()).unwrap();
        let mut resp = String::new();
        let _ = s.read_to_string(&mut resp);
        assert!(resp.starts_with("ERR"), "{bad:?} -> {resp:?}");
    }
    // still serving afterwards
    assert_eq!(transfer::list_peer(&addr, None, None).unwrap().len(), 1);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}
