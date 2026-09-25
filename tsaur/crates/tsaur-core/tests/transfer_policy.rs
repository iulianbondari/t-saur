//! Authorization policy of the volume exchange (`docs/design/VOLUME-TRUST.md` §6.3, §6.4):
//! revoked fingerprints fail the handshake on either side even when pinned or allowed, and a
//! client restricted to some sets is answered like a stranger for the others (`404 unknown
//! set`), so it learns nothing about them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tsaur_core::tls::{self, Identity, Revocations};
use tsaur_core::transfer::{self, Acl, ClientTls, FetchOptions, Server, ServerLimits, Want};
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
    vol_dir: PathBuf,
    set_id: [u8; 32],
    descriptor_b3: [u8; 32],
}

/// One archive split 2+1 with 64 KiB pieces under `<root>/<tag>`.
fn lab(root: &Path, tag: &str) -> Lab {
    let dir = root.join(tag);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x9a7c_3e51_0b2d_4f68u64 ^ (tag.len() as u64) << 3;
    let big: Vec<u8> = (0..300_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
    std::fs::write(src.join(format!("{tag}.txt")), format!("set {tag}\n").repeat(1500)).unwrap();
    let archive = dir.join(format!("{tag}.tsr"));
    pack(std::slice::from_ref(&src), &archive, PackOptions::default()).unwrap();
    let vol_dir = dir.join("volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&archive, &SplitOptions { data: 2, parity: 1, piece_size: Some(65536), outputs: vec![vol_dir.clone()] }).unwrap();
    let mut set_id = [0u8; 32];
    set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    let mut descriptor_b3 = [0u8; 32];
    descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    Lab { dir, archive_bytes: std::fs::read(&archive).unwrap(), vol_dir, set_id, descriptor_b3 }
}

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tsaur-policy-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn identity(dir: &Path, name: &str) -> Identity {
    tls::generate(&dir.join(name)).unwrap()
}

/// Serve over TLS as `id` with an authorization policy: the handshake admits `acl.admitted()`
/// minus `revoked`, the requests are filtered by `acl`.
fn serve(paths: &[PathBuf], id: &Identity, acl: Acl, revoked: &Revocations) -> (String, Arc<AtomicBool>) {
    let admitted: Vec<[u8; 32]> = acl.admitted().into_iter().filter(|fp| !revoked.contains(fp)).collect();
    let cfg = tls::server_config_with(id, &admitted, revoked).unwrap();
    let server = Server::bind_acl(paths, "127.0.0.1:0", ServerLimits::default(), Some(cfg), acl).unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn opts(l: &Lab, peer: &str, out: &Path, tls: ClientTls) -> FetchOptions {
    FetchOptions {
        set_id: l.set_id,
        descriptor_b3: Some(l.descriptor_b3),
        peers: vec![peer.to_string()],
        out_dir: out.to_path_buf(),
        local: Vec::new(),
        want: Want::Needed,
        max_pieces: None,
        tls: Some(tls),
    }
}

fn pinned(identity: &Identity, peer: &Identity) -> ClientTls {
    ClientTls { identity: Some(identity.clone()), peer_ids: vec![peer.fingerprint], ..Default::default() }
}

fn assert_join(l: &Lab, out: &Path) {
    let joined = l.dir.join("joined.tsr");
    volumes::join(std::slice::from_ref(&out.to_path_buf()), &joined).unwrap();
    assert_eq!(std::fs::read(&joined).unwrap(), l.archive_bytes);
    let mut r = Reader::open(&joined, None).unwrap();
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
}

fn no_volumes_written(dir: &Path) -> bool {
    !dir.is_dir() || std::fs::read_dir(dir).unwrap().all(|e| !e.unwrap().path().to_string_lossy().ends_with(".tsrv"))
}

#[test]
fn revoked_client_is_closed_at_the_handshake_even_when_allowed() {
    let dir = root("revoke-client");
    let l = lab(&dir, "one");
    let server_id = identity(&dir, "server.key");
    let a = identity(&dir, "a.key");
    let b = identity(&dir, "b.key");
    // both are allowed, B is revoked: the revocation wins
    let revoked: Revocations = [b.fingerprint].into_iter().collect();
    let (addr, stop) = serve(std::slice::from_ref(&l.vol_dir), &server_id, Acl::new([a.fingerprint, b.fingerprint], []), &revoked);
    let out_b = dir.join("here-b");
    let err = transfer::fetch(&opts(&l, &addr, &out_b, pinned(&b, &server_id))).unwrap_err().to_string();
    assert!(err.contains("no peer supplied an acceptable descriptor"), "{err}");
    assert!(!err.contains("ERR "), "a refusal at the handshake carries no protocol reply: {err}");
    assert!(no_volumes_written(&out_b));
    assert!(transfer::list_peer(&addr, Some(&b), Some(server_id.fingerprint)).is_err());
    // A is unaffected
    let out_a = dir.join("here-a");
    let rep = transfer::fetch(&opts(&l, &addr, &out_a, pinned(&a, &server_id))).unwrap();
    assert!(rep.reconstructible && rep.encrypted, "{rep:?}");
    assert_join(&l, &out_a);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revoked_server_is_refused_by_the_client_before_any_request() {
    let dir = root("revoke-server");
    let l = lab(&dir, "one");
    let server_id = identity(&dir, "server.key");
    let a = identity(&dir, "a.key");
    let (addr, stop) = serve(std::slice::from_ref(&l.vol_dir), &server_id, Acl::new([a.fingerprint], []), &Revocations::default());
    let revoked: Revocations = [server_id.fingerprint].into_iter().collect();
    // the pinned peer is revoked: the fetch is refused as a policy error before any connection
    let out = dir.join("here");
    let client = ClientTls { identity: Some(a.clone()), peer_ids: vec![server_id.fingerprint], revoked: revoked.clone() };
    let err = transfer::fetch(&opts(&l, &addr, &out, client)).unwrap_err();
    assert!(matches!(err, Error::Policy(_)) && err.to_string().contains("is revoked"), "{err}");
    assert!(no_volumes_written(&out));
    let err = transfer::list_peer_with(&addr, Some(&a), Some(server_id.fingerprint), &revoked).unwrap_err();
    assert!(matches!(err, Error::Policy(_)), "{err}");
    // and the verifier refuses the same certificate at the handshake when a caller builds the
    // configuration directly (defence in depth behind the start-up check)
    let cfg = tls::client_config_with(Some(&a), server_id.fingerprint, &revoked).unwrap();
    let conn = rustls::ClientConnection::new(cfg, tls::server_name()).unwrap();
    let sock = std::net::TcpStream::connect(&addr).unwrap();
    let mut session = rustls::StreamOwned::new(conn, sock);
    let handshake = session.conn.complete_io(&mut session.sock);
    assert!(handshake.is_err() || session.conn.is_handshaking(), "a revoked server certificate must not complete the handshake");
    // the server itself is fine: a client without the revocation fetches and joins
    let rep = transfer::fetch(&opts(&l, &addr, &out, pinned(&a, &server_id))).unwrap();
    assert!(rep.reconstructible, "{rep:?}");
    assert_join(&l, &out);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn client_restricted_to_one_set_sees_only_that_set() {
    let dir = root("per-set");
    let l1 = lab(&dir, "first");
    let l2 = lab(&dir, "second");
    assert_ne!(l1.set_id, l2.set_id);
    let server_id = identity(&dir, "server.key");
    let a = identity(&dir, "a.key");
    let b = identity(&dir, "b.key");
    let c = identity(&dir, "c.key");
    // A may read everything, B only the first set, C is not listed at all
    let acl = Acl::new([a.fingerprint], [(l1.set_id, b.fingerprint)]);
    assert_eq!(acl.admitted().len(), 2);
    assert_eq!(acl.restricted(), vec![b.fingerprint]);
    assert_eq!(acl.readers_of(&l1.set_id), Some(2));
    assert_eq!(acl.readers_of(&l2.set_id), Some(1));
    let (addr, stop) = serve(&[l1.vol_dir.clone(), l2.vol_dir.clone()], &server_id, acl, &Revocations::default());
    // B lists one set, fetches it, and is told the other one does not exist
    let sets = transfer::list_peer(&addr, Some(&b), Some(server_id.fingerprint)).unwrap();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0].0, hex::encode(l1.set_id));
    let out_b1 = dir.join("b-first");
    let rep = transfer::fetch(&opts(&l1, &addr, &out_b1, pinned(&b, &server_id))).unwrap();
    assert!(rep.reconstructible, "{rep:?}");
    assert_join(&l1, &out_b1);
    let out_b2 = dir.join("b-second");
    let err = transfer::fetch(&opts(&l2, &addr, &out_b2, pinned(&b, &server_id))).unwrap_err().to_string();
    assert!(err.contains("404 unknown set"), "an unauthorised set is answered like an unknown one: {err}");
    assert!(no_volumes_written(&out_b2));
    // A sees and fetches both
    assert_eq!(transfer::list_peer(&addr, Some(&a), Some(server_id.fingerprint)).unwrap().len(), 2);
    for (l, out) in [(&l1, dir.join("a-first")), (&l2, dir.join("a-second"))] {
        let rep = transfer::fetch(&opts(l, &addr, &out, pinned(&a, &server_id))).unwrap();
        assert!(rep.reconstructible, "{rep:?}");
        assert_join(l, &out);
    }
    // C is refused at the handshake: no reply, nothing written
    let out_c = dir.join("c-first");
    let err = transfer::fetch(&opts(&l1, &addr, &out_c, pinned(&c, &server_id))).unwrap_err().to_string();
    assert!(!err.contains("ERR "), "{err}");
    assert!(no_volumes_written(&out_c));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn per_set_lists_need_tls_and_everyone_is_the_plain_default() {
    let dir = root("plain-acl");
    let l = lab(&dir, "one");
    let restricted = Acl::new([], [(l.set_id, [7u8; 32])]);
    let err = Server::bind_acl(std::slice::from_ref(&l.vol_dir), "127.0.0.1:0", ServerLimits::default(), None, restricted).err().unwrap();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    let server = Server::bind(std::slice::from_ref(&l.vol_dir), "127.0.0.1:0").unwrap();
    assert_eq!(server.acl().readers_of(&l.set_id), None, "a plain server has no client identities: every set is readable");
    assert!(server.acl().may_read(None, &l.set_id));
    let _ = std::fs::remove_dir_all(&dir);
}
