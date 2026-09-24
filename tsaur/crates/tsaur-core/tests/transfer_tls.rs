//! The volume exchange over TLS with locally pinned identities: two self-signed certificates,
//! each side accepting only the fingerprint it was told through a trusted channel. No CA, no
//! account, no service: a mismatch on either side ends the handshake before any request byte,
//! and a plain client against a TLS server (or the reverse) fails cleanly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tsaur_core::tls::{self, Identity};
use tsaur_core::transfer::{self, ClientTls, FetchOptions, Server, ServerLimits, Want};
use tsaur_core::volumes::{self, SplitOptions};
use tsaur_core::{pack, PackOptions, Reader};

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

fn lab(tag: &str) -> Lab {
    let dir = std::env::temp_dir().join(format!("tsaur-tls-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x5eed_c0de_1234_5678u64 ^ tag.len() as u64;
    let big: Vec<u8> = (0..400_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
    std::fs::write(src.join("notes.txt"), "only the pinned peer is heard\n".repeat(2000)).unwrap();
    let archive = dir.join("set.tsr");
    pack(std::slice::from_ref(&src), &archive, PackOptions::default()).unwrap();
    let vol_dir = dir.join("src-volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&archive, &SplitOptions { data: 2, parity: 1, piece_size: Some(65536), outputs: vec![vol_dir] }).unwrap();
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    let mut descriptor_b3 = [0u8; 32];
    descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    Lab { dir, archive_bytes: std::fs::read(&archive).unwrap(), volumes: rep.volumes.iter().map(|v| v.path.clone()).collect(), set_id, descriptor_b3 }
}

fn identity(dir: &Path, name: &str) -> Identity {
    tls::generate(&dir.join(name)).unwrap()
}

/// Serve over TLS as `id`, admitting the listed client fingerprints (none = any client).
fn serve_tls(paths: &[PathBuf], id: &Identity, allowed: &[[u8; 32]]) -> (String, Arc<AtomicBool>) {
    let cfg = tls::server_config(id, allowed).unwrap();
    let server = Server::bind_tls(paths, "127.0.0.1:0", ServerLimits::default(), Some(cfg)).unwrap();
    assert!(server.encrypted());
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn serve_plain(paths: &[PathBuf]) -> (String, Arc<AtomicBool>) {
    let server = Server::bind(paths, "127.0.0.1:0").unwrap();
    assert!(!server.encrypted());
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn opts(l: &Lab, peer: &str, out: &Path, tls: Option<ClientTls>) -> FetchOptions {
    FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers: vec![peer.to_string()], out_dir: out.to_path_buf(), local: Vec::new(), want: Want::Needed, max_pieces: None, tls }
}

fn pinned(identity: Option<&Identity>, peer: &Identity) -> Option<ClientTls> {
    Some(ClientTls { identity: identity.cloned(), peer_ids: vec![peer.fingerprint] })
}

fn assert_join(l: &Lab, inputs: &[PathBuf], out: &Path) {
    volumes::join(inputs, out).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), l.archive_bytes);
    let mut r = Reader::open(out, None).unwrap();
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
}

fn no_volumes_written(dir: &Path) -> bool {
    !dir.is_dir() || std::fs::read_dir(dir).unwrap().all(|e| !e.unwrap().path().to_string_lossy().ends_with(".tsrv"))
}

#[test]
fn identity_files_roundtrip_and_fingerprints_parse() {
    let l = lab("ident");
    let a = identity(&l.dir, "a.key");
    assert!(l.dir.join("a.key").is_file() && l.dir.join("a.key.crt").is_file());
    let key_pem = std::fs::read_to_string(l.dir.join("a.key")).unwrap();
    assert!(key_pem.contains("PRIVATE KEY"));
    let again = tls::load(&l.dir.join("a.key")).unwrap();
    assert_eq!(again.fingerprint, a.fingerprint);
    assert_eq!(again.fingerprint, tls::fingerprint(&again.cert));
    let b = identity(&l.dir, "b.key");
    assert_ne!(a.fingerprint, b.fingerprint, "each identity is fresh");
    let hex_fp = hex::encode(a.fingerprint);
    assert_eq!(tls::parse_fingerprint(&hex_fp).unwrap(), a.fingerprint);
    let colons: Vec<String> = hex_fp.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).to_uppercase()).collect();
    assert_eq!(tls::parse_fingerprint(&format!(" {} \n", colons.join(":"))).unwrap(), a.fingerprint);
    assert!(tls::parse_fingerprint("abc").is_err());
    assert!(tls::parse_fingerprint(&"zz".repeat(32)).is_err());
    assert!(tls::load(&l.dir.join("missing.key")).is_err());
    std::fs::write(l.dir.join("junk.key"), "not a key").unwrap();
    std::fs::write(l.dir.join("junk.key.crt"), "not a certificate").unwrap();
    assert!(tls::load(&l.dir.join("junk.key")).is_err());
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn mutual_tls_fetch_with_pinned_identities_joins_offline() {
    let l = lab("mutual");
    let server_id = identity(&l.dir, "server.key");
    let client_id = identity(&l.dir, "client.key");
    let (addr, stop) = serve_tls(&l.volumes, &server_id, &[client_id.fingerprint]);
    let sets = transfer::list_peer(&addr, Some(&client_id), Some(server_id.fingerprint)).unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].0, hex::encode(l.set_id));
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, pinned(Some(&client_id), &server_id))).unwrap();
    assert!(rep.encrypted);
    assert_eq!(rep.verified_against, "descriptor");
    assert_eq!(rep.volumes_completed.len(), 2);
    assert_eq!(rep.pieces_rejected, 0);
    assert!(rep.reconstructible);
    assert_eq!(rep.peers_used, vec![addr.clone()]);
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn server_with_another_certificate_is_refused_before_any_request() {
    let l = lab("wrongpeer");
    let server_id = identity(&l.dir, "server.key");
    let client_id = identity(&l.dir, "client.key");
    let impostor = identity(&l.dir, "impostor.key");
    // the server really is `impostor`; the client was told to expect `server_id`
    let (addr, stop) = serve_tls(&l.volumes, &impostor, &[client_id.fingerprint]);
    let out = l.dir.join("here");
    let err = transfer::fetch(&opts(&l, &addr, &out, pinned(Some(&client_id), &server_id))).unwrap_err().to_string();
    assert!(err.contains("does not match the pinned identity"), "{err}");
    assert!(no_volumes_written(&out));
    let err = transfer::list_peer(&addr, Some(&client_id), Some(server_id.fingerprint)).unwrap_err().to_string();
    assert!(err.contains("does not match the pinned identity"), "{err}");
    // the honest server, pinned correctly, works from the same client
    let (good, stop2) = serve_tls(&l.volumes, &server_id, &[client_id.fingerprint]);
    let rep = transfer::fetch(&opts(&l, &good, &out, pinned(Some(&client_id), &server_id))).unwrap();
    assert!(rep.reconstructible && rep.encrypted);
    assert_join(&l, &[out], &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    stop2.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn clients_outside_the_allow_list_are_refused() {
    let l = lab("allow");
    let server_id = identity(&l.dir, "server.key");
    let client_id = identity(&l.dir, "client.key");
    let stranger = identity(&l.dir, "stranger.key");
    let (addr, stop) = serve_tls(&l.volumes, &server_id, &[client_id.fingerprint]);
    let out = l.dir.join("here");
    // an identity that is not on the list
    let err = transfer::fetch(&opts(&l, &addr, &out, pinned(Some(&stranger), &server_id))).unwrap_err().to_string();
    assert!(err.contains("no peer supplied an acceptable descriptor"), "{err}");
    assert!(no_volumes_written(&out));
    // no identity at all
    let err = transfer::fetch(&opts(&l, &addr, &out, pinned(None, &server_id))).unwrap_err().to_string();
    assert!(err.contains("no peer supplied an acceptable descriptor"), "{err}");
    assert!(no_volumes_written(&out));
    // the listed identity is served
    let rep = transfer::fetch(&opts(&l, &addr, &out, pinned(Some(&client_id), &server_id))).unwrap();
    assert!(rep.reconstructible);
    assert_join(&l, &[out], &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn server_authenticated_only_mode_accepts_anonymous_clients() {
    let l = lab("anon");
    let server_id = identity(&l.dir, "server.key");
    let (addr, stop) = serve_tls(&l.volumes, &server_id, &[]);
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, pinned(None, &server_id))).unwrap();
    assert!(rep.reconstructible && rep.encrypted);
    assert_join(&l, std::slice::from_ref(&out), &l.dir.join("joined.tsr"));
    // the server is still pinned: a wrong expectation is refused even in this mode
    let other = identity(&l.dir, "other.key");
    let err = transfer::fetch(&opts(&l, &addr, &l.dir.join("elsewhere"), pinned(None, &other))).unwrap_err().to_string();
    assert!(err.contains("does not match the pinned identity"), "{err}");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn plain_and_tls_sides_do_not_mix_and_nothing_panics() {
    let l = lab("mix");
    let server_id = identity(&l.dir, "server.key");
    let client_id = identity(&l.dir, "client.key");
    // plain client against a TLS server
    let (tls_addr, stop_tls) = serve_tls(&l.volumes, &server_id, &[client_id.fingerprint]);
    let out = l.dir.join("here");
    let err = transfer::fetch(&opts(&l, &tls_addr, &out, None)).unwrap_err().to_string();
    assert!(err.contains("no peer supplied an acceptable descriptor"), "{err}");
    assert!(no_volumes_written(&out));
    assert!(transfer::list_peer(&tls_addr, None, None).is_err());
    // TLS client against a plain server
    let (plain_addr, stop_plain) = serve_plain(&l.volumes);
    let err = transfer::fetch(&opts(&l, &plain_addr, &out, pinned(Some(&client_id), &server_id))).unwrap_err().to_string();
    assert!(err.contains("no peer supplied an acceptable descriptor"), "{err}");
    assert!(no_volumes_written(&out));
    assert!(transfer::list_peer(&plain_addr, Some(&client_id), Some(server_id.fingerprint)).is_err());
    // both servers still answer their own kind afterwards
    assert_eq!(transfer::list_peer(&plain_addr, None, None).unwrap().len(), 1);
    assert_eq!(transfer::list_peer(&tls_addr, Some(&client_id), Some(server_id.fingerprint)).unwrap().len(), 1);
    // one identity per peer is required
    let bad = FetchOptions { peers: vec![tls_addr.clone(), plain_addr.clone()], ..opts(&l, &tls_addr, &out, pinned(Some(&client_id), &server_id)) };
    let err = transfer::fetch(&bad).unwrap_err().to_string();
    assert!(err.contains("one identity per peer"), "{err}");
    stop_tls.store(true, Ordering::Relaxed);
    stop_plain.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn interrupted_tls_fetch_resumes_over_tls() {
    let l = lab("resume");
    let server_id = identity(&l.dir, "server.key");
    let client_id = identity(&l.dir, "client.key");
    let (addr, stop) = serve_tls(&l.volumes, &server_id, &[client_id.fingerprint]);
    let out = l.dir.join("here");
    let first = transfer::fetch(&FetchOptions { max_pieces: Some(3), ..opts(&l, &addr, &out, pinned(Some(&client_id), &server_id)) }).unwrap();
    assert!(first.stopped_early);
    assert_eq!(first.pieces_received, 3);
    let second = transfer::fetch(&opts(&l, &addr, &out, pinned(Some(&client_id), &server_id))).unwrap();
    assert!(!second.stopped_early);
    assert_eq!(second.pieces_reverified, 3, "the pieces on disk are hash-checked, not trusted");
    assert!(second.reconstructible);
    assert_join(&l, &[out], &l.dir.join("joined.tsr"));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}
