//! Resume after interruption is decided by hashes read from disk, never by the progress map;
//! resource limits and malformed peers are handled without panics or stray files.

use serde::Serialize;
use serde_bytes::ByteBuf;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tsaur_core::transfer::{self, FetchOptions, Server, ServerLimits, Want};
use tsaur_core::volumes::{self, SplitOptions, VolumeSet, HEADER_LEN};
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
    set: VolumeSet,
    set_id: [u8; 32],
    descriptor_b3: [u8; 32],
}

const PS: usize = 65536;

fn lab(tag: &str) -> Lab {
    let dir = std::env::temp_dir().join(format!("tsaur-resume-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x7e57_0000_0000_0001u64 ^ (tag.len() as u64) << 8;
    let big: Vec<u8> = (0..900_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
    let archive = dir.join("set.tsr");
    pack(std::slice::from_ref(&src), &archive, PackOptions::default()).unwrap();
    let vol_dir = dir.join("src-volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&archive, &SplitOptions { data: 4, parity: 2, piece_size: Some(PS as u32), outputs: vec![vol_dir.clone()] }).unwrap();
    let set = volumes::inspect(&[vol_dir], false).unwrap().remove(0).set;
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    let mut descriptor_b3 = [0u8; 32];
    descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    Lab { dir, archive_bytes: std::fs::read(&archive).unwrap(), volumes: rep.volumes.iter().map(|v| v.path.clone()).collect(), set, set_id, descriptor_b3 }
}

fn serve(paths: &[PathBuf], limits: ServerLimits) -> (String, Arc<AtomicBool>) {
    let server = Server::bind_with(paths, "127.0.0.1:0", limits).unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn opts(l: &Lab, peers: Vec<String>, out: &Path) -> FetchOptions {
    FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers, out_dir: out.to_path_buf(), local: Vec::new(), want: Want::Needed, max_pieces: None, tls: None }
}

fn assert_join(l: &Lab, inputs: &[PathBuf], out: &Path) {
    volumes::join(inputs, out).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), l.archive_bytes);
    let mut r = Reader::open(out, None).unwrap();
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
}

/// The `.partial` file and its map of the volume that was in progress when the fetch stopped.
fn in_progress(out: &Path) -> (PathBuf, PathBuf) {
    let partial = std::fs::read_dir(out).unwrap().map(|e| e.unwrap().path()).find(|p| p.to_string_lossy().ends_with(".partial")).expect("a partial volume");
    let map = PathBuf::from(format!("{}.map", partial.display()));
    assert!(map.is_file(), "map next to the partial file");
    (partial, map)
}

/// Same shape as the private map inside the crate (a CBOR map with these three fields).
#[derive(Serialize)]
struct MapLike {
    descriptor_b3: ByteBuf,
    index: u16,
    have: Vec<bool>,
}

/// Interrupt a fetch after 6 pieces: volume 1 complete (4 pieces), volume 2 half done.
fn interrupted(l: &Lab, addr: &str, out: &Path) -> transfer::FetchReport {
    let mut o = opts(l, vec![addr.to_string()], out);
    o.max_pieces = Some(6);
    let rep = transfer::fetch(&o).unwrap();
    assert!(rep.stopped_early && rep.pieces_received == 6, "{rep:?}");
    rep
}

#[test]
fn damaged_piece_claimed_by_the_map_is_fetched_again() {
    let l = lab("damaged");
    let (addr, stop) = serve(&l.volumes, ServerLimits::default());
    let out = l.dir.join("here");
    interrupted(&l, &addr, &out);
    let (partial, _map) = in_progress(&out);
    // flip a byte inside the first piece of the partial volume (the map says it is complete)
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&partial).unwrap();
    f.seek(SeekFrom::Start(HEADER_LEN as u64 + 100)).unwrap();
    let mut b = [0u8; 1];
    f.read_exact(&mut b).unwrap();
    f.seek(SeekFrom::Start(HEADER_LEN as u64 + 100)).unwrap();
    f.write_all(&[b[0] ^ 0xFF]).unwrap();
    drop(f);
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out)).unwrap();
    assert_eq!(rep.pieces_reverify_failed, 1, "the damaged piece must be detected on resume");
    assert_eq!(rep.pieces_reverified, 1, "the intact piece is kept");
    let remaining: usize = (0..4).map(|vi| l.set.volume_pieces(vi) as usize).sum::<usize>() - 6;
    assert_eq!(rep.pieces_received, remaining + 1);
    assert!(rep.reconstructible && rep.volumes_partial.is_empty());
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn damaged_or_missing_map_falls_back_to_checking_every_piece_on_disk() {
    let l = lab("map");
    let (addr, stop) = serve(&l.volumes, ServerLimits::default());
    let out = l.dir.join("here");
    interrupted(&l, &addr, &out);
    let (_partial, map) = in_progress(&out);
    // half a map (interrupted while it was being written)
    let bytes = std::fs::read(&map).unwrap();
    std::fs::write(&map, &bytes[..bytes.len() / 2]).unwrap();
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out)).unwrap();
    assert_eq!(rep.pieces_reverified, 2, "both pieces already on disk verify");
    assert_eq!(rep.pieces_reverify_failed, 0);
    let remaining: usize = (0..4).map(|vi| l.set.volume_pieces(vi) as usize).sum::<usize>() - 6;
    assert_eq!(rep.pieces_received, remaining, "nothing intact is fetched twice");
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // no map at all
    let out2 = l.dir.join("here2");
    interrupted(&l, &addr, &out2);
    let (_partial, map) = in_progress(&out2);
    std::fs::remove_file(&map).unwrap();
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out2)).unwrap();
    assert_eq!(rep.pieces_reverified, 2);
    assert_eq!(rep.pieces_received, remaining);
    assert_join(&l, std::slice::from_ref(&out2), &l.dir.join("joined2.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn map_claiming_pieces_that_were_never_written_is_not_believed() {
    let l = lab("liarmap");
    let (addr, stop) = serve(&l.volumes, ServerLimits::default());
    let out = l.dir.join("here");
    interrupted(&l, &addr, &out);
    let (_partial, map) = in_progress(&out);
    // a map that claims every piece of the volume, with the right descriptor hash and index
    let count = l.set.volume_pieces(1) as usize;
    let fake = MapLike { descriptor_b3: ByteBuf::from(l.descriptor_b3.to_vec()), index: 1, have: vec![true; count] };
    std::fs::write(&map, tsaur_core::format::cbor_encode(&fake).unwrap()).unwrap();
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out)).unwrap();
    assert_eq!(rep.pieces_reverified, 2);
    assert_eq!(rep.pieces_reverify_failed, count - 2, "claimed but unwritten pieces fail the disk check");
    assert!(rep.reconstructible);
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn truncated_partial_and_foreign_partial_are_handled() {
    let l = lab("trunc");
    let (addr, stop) = serve(&l.volumes, ServerLimits::default());
    let out = l.dir.join("here");
    interrupted(&l, &addr, &out);
    let (partial, _map) = in_progress(&out);
    // interrupted in the middle of writing the second piece: the file is short
    let f = std::fs::OpenOptions::new().write(true).open(&partial).unwrap();
    f.set_len(HEADER_LEN as u64 + PS as u64 + PS as u64 / 2).unwrap();
    drop(f);
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out)).unwrap();
    assert_eq!(rep.pieces_reverified, 1);
    assert_eq!(rep.pieces_reverify_failed, 1);
    assert!(rep.reconstructible);
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // a partial file whose header belongs to another volume index is discarded, not trusted
    let out2 = l.dir.join("here2");
    interrupted(&l, &addr, &out2);
    let (partial, _map) = in_progress(&out2);
    let mut bytes = std::fs::read(&partial).unwrap();
    bytes[6] = 3; // volume index field of the header
    std::fs::write(&partial, &bytes).unwrap();
    let rep = transfer::fetch(&opts(&l, vec![addr.clone()], &out2)).unwrap();
    assert_eq!(rep.pieces_reverified, 0);
    assert!(rep.reconstructible);
    assert_join(&l, std::slice::from_ref(&out2), &l.dir.join("joined2.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn server_refuses_connections_beyond_its_limits() {
    let l = lab("limits");
    let limits = ServerLimits { max_connections: 64, max_connections_per_peer: 3, timeout: std::time::Duration::from_secs(5), ..ServerLimits::default() };
    let (addr, stop) = serve(&l.volumes, limits);
    // three idle connections occupy the per-peer allowance; the fourth is turned away at once
    let idle: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(&addr).unwrap()).collect();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut fourth = TcpStream::connect(&addr).unwrap();
    fourth.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut line = String::new();
    BufReader::new(&mut fourth).read_line(&mut line).unwrap();
    assert!(line.starts_with("ERR 503"), "{line:?}");
    drop(idle);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(transfer::list_peer(&addr, None, None).unwrap().len(), 1, "capacity is released when connections close");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

/// A peer that answers every request with the given raw bytes.
fn raw_peer(reply: Vec<u8>) -> (String, Arc<AtomicBool>) {
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
                let _ = s.write_all(&reply);
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    });
    (addr, stop)
}

#[test]
fn oversized_and_malformed_replies_are_refused_without_allocation_or_files() {
    let l = lab("malformed");
    let cases: Vec<Vec<u8>> = vec![
        b"OK 99999999999\n".to_vec(),              // far above the descriptor limit: refused before allocation
        b"OK 5\nab".to_vec(),                      // shorter than announced
        b"HELLO\n".to_vec(),                       // not a status line
        vec![b'O'; 1000],                          // over-long line without newline
        b"OK 3\n\xff\xff\xff".to_vec(),            // not CBOR
        format!("OK {}\n", 64 << 20).into_bytes(), // exactly the limit but nothing follows
    ];
    for (i, reply) in cases.into_iter().enumerate() {
        let (addr, stop) = raw_peer(reply);
        let out = l.dir.join(format!("out{i}"));
        let err = transfer::fetch(&opts(&l, vec![addr], &out)).unwrap_err();
        assert!(matches!(err, Error::Missing(_)), "case {i}: {err}");
        assert_eq!(std::fs::read_dir(&out).map(|d| d.count()).unwrap_or(0), 0, "case {i}: nothing written");
        stop.store(true, Ordering::Relaxed);
    }
    let _ = std::fs::remove_dir_all(&l.dir);
}

/// A peer serving the real pieces but a descriptor whose archive name is chosen by the attacker.
fn renaming_peer(l: &Lab, name: &str) -> (String, Arc<AtomicBool>) {
    let mut set = l.set.clone();
    set.archive_name = name.to_string();
    let descriptor = tsaur_core::format::cbor_encode(&set).unwrap();
    let vols = l.volumes.clone();
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
                let parts: Vec<&str> = line.trim().split(' ').collect();
                let body: Vec<u8> = match parts.get(1) {
                    Some(&"DESCRIPTOR") => descriptor.clone(),
                    Some(&"HAVE") => vec![0x86, 0, 1, 2, 3, 4, 5],
                    Some(&"PIECE") => {
                        let vi: usize = parts[3].parse().unwrap();
                        let st: u64 = parts[4].parse().unwrap();
                        let mut f = std::fs::File::open(&vols[vi]).unwrap();
                        f.seek(SeekFrom::Start(HEADER_LEN as u64 + st * PS as u64)).unwrap();
                        let mut b = vec![0u8; PS];
                        f.read_exact(&mut b).unwrap();
                        b
                    }
                    _ => Vec::new(),
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
fn archive_names_from_the_network_never_choose_local_paths() {
    let l = lab("names");
    // a reserved device name and a dot-dot name: accepted as data, never used as a file name
    for (i, name) in ["NUL", "..", "trailing.", "con.tsr"].iter().enumerate() {
        let (addr, stop) = renaming_peer(&l, name);
        let out = l.dir.join(format!("out{i}"));
        let mut o = opts(&l, vec![addr], &out);
        o.descriptor_b3 = None; // the descriptor hash is different, so only the set id is known
        let rep = transfer::fetch(&o).unwrap();
        assert!(rep.reconstructible, "{name}: {rep:?}");
        for entry in std::fs::read_dir(&out).unwrap() {
            let file = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(file.starts_with("set-") && file.ends_with(".tsrv"), "{name}: unexpected local name {file}");
        }
        assert_join(&l, std::slice::from_ref(&out), &l.dir.join(format!("joined{i}.tsr")));
        stop.store(true, Ordering::Relaxed);
    }
    // a name with a separator is rejected outright by descriptor validation
    let (addr, stop) = renaming_peer(&l, "../escape");
    let mut o = opts(&l, vec![addr], &l.dir.join("never"));
    o.descriptor_b3 = None;
    assert!(matches!(transfer::fetch(&o).unwrap_err(), Error::Missing(_)));
    assert!(!l.dir.join("never").join("escape").exists());
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}
