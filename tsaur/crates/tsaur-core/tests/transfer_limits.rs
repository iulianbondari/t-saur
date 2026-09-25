//! Robustness of the serving side against clients that are slow, idle or too frequent: idle and
//! trickling connections lose their slot when their time budget ends, the per-address request
//! rate is bounded (`ERR 429` in plain mode, a silent close in TLS mode), and a well-behaved
//! fetch still completes against a rate-limited server because the client backs off.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsaur_core::tls::{self, Identity};
use tsaur_core::transfer::{self, ClientTls, FetchOptions, Server, ServerLimits, Want};
use tsaur_core::volumes::{self, SplitOptions};
use tsaur_core::{pack, PackOptions};

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
    let dir = std::env::temp_dir().join(format!("tsaur-limits-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut seed = 0x7a5e_11d0_9c3b_2f41u64 ^ tag.len() as u64;
    let big: Vec<u8> = (0..500_000).map(|_| (xorshift(&mut seed) >> 56) as u8).collect();
    std::fs::write(src.join("noise.bin"), &big).unwrap();
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

fn serve(paths: &[PathBuf], limits: ServerLimits, tls: Option<(&Identity, &[[u8; 32]])>) -> (String, Arc<AtomicBool>) {
    let cfg = tls.map(|(id, allowed)| tls::server_config(id, allowed).unwrap());
    let server = Server::bind_tls(paths, "127.0.0.1:0", limits, cfg).unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || server.run(flag).unwrap());
    (addr, stop)
}

fn opts(l: &Lab, peer: &str, out: &Path, tls: Option<ClientTls>) -> FetchOptions {
    FetchOptions { set_id: l.set_id, descriptor_b3: Some(l.descriptor_b3), peers: vec![peer.to_string()], out_dir: out.to_path_buf(), local: Vec::new(), want: Want::Needed, max_pieces: None, tls }
}

/// Open a raw connection, send `line`, return the status line (or the error).
fn raw(addr: &str, line: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(line.as_bytes())?;
    let mut status = String::new();
    BufReader::new(&mut s).read_line(&mut status)?;
    Ok(status)
}

#[test]
fn idle_and_trickling_clients_lose_their_slots_when_the_budget_ends() {
    let l = lab("slow");
    let limits = ServerLimits { max_connections: 2, max_connections_per_peer: 2, timeout: Duration::from_secs(1), ..ServerLimits::default() };
    let (addr, stop) = serve(&l.volumes, limits, None);
    let started = Instant::now();
    // one connection that never sends anything ...
    let mut idle = TcpStream::connect(&addr).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // ... and one that trickles a byte every 200 ms (each byte alone would reset a plain read timeout)
    let mut trickle = TcpStream::connect(&addr).unwrap();
    trickle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut feeder = trickle.try_clone().unwrap();
    let feeding = std::thread::spawn(move || {
        for _ in 0..15 {
            if feeder.write_all(b"T").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    std::thread::sleep(Duration::from_millis(200));
    // both slots are taken: a third connection is turned away at once
    let third = raw(&addr, "TSXP/1 SETS\n").unwrap();
    assert!(third.starts_with("ERR 503"), "{third:?}");
    // when the budget (1 s) ends, both are closed and the slots are free again
    let mut sink = Vec::new();
    let _ = idle.read_to_end(&mut sink);
    let mut sink = Vec::new();
    let _ = trickle.read_to_end(&mut sink);
    let closed_after = started.elapsed();
    assert!(closed_after < Duration::from_secs(4), "slow connections were held for {closed_after:?}");
    let ok = raw(&addr, "TSXP/1 SETS\n").unwrap();
    assert!(ok.starts_with("OK "), "{ok:?}");
    assert_eq!(transfer::list_peer(&addr, None, None).unwrap().len(), 1);
    feeding.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn request_rate_per_address_is_limited_and_a_fetch_still_completes() {
    let l = lab("rate");
    let limits = ServerLimits { max_requests_per_second: 5, ..ServerLimits::default() };
    let (addr, stop) = serve(&l.volumes, limits, None);
    // a burst of 30 requests from one address: about twice the rate is admitted, the rest is 429
    let t0 = Instant::now();
    let mut ok = 0;
    let mut refused = 0;
    for _ in 0..30 {
        let status = raw(&addr, "TSXP/1 SETS\n").unwrap();
        if status.starts_with("OK ") {
            ok += 1;
        } else {
            assert!(status.starts_with("ERR 429"), "{status:?}");
            refused += 1;
        }
    }
    let burst_secs = t0.elapsed().as_secs_f64();
    // the bucket starts full (a burst of 10) and refills at 5 per second while the burst runs;
    // on a slow machine the burst itself takes seconds, so the bound is computed from the clock
    let admitted_at_most = 10 + (burst_secs * 5.0).ceil() as usize + 1;
    assert!(ok >= 10 && ok <= admitted_at_most, "ok {ok}, refused {refused}, burst took {burst_secs:.2} s");
    assert!(refused >= 30usize.saturating_sub(admitted_at_most), "ok {ok}, refused {refused}, burst took {burst_secs:.2} s");
    if burst_secs < 1.0 {
        assert!(refused >= 10, "a fast burst must visibly hit the limit: ok {ok}, refused {refused}, burst took {burst_secs:.2} s");
    }
    // tokens come back with time
    std::thread::sleep(Duration::from_millis(1200));
    assert!(raw(&addr, "TSXP/1 SETS\n").unwrap().starts_with("OK "));
    // a real fetch (descriptor + have + 8 pieces) completes because the client backs off on 429
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, None)).unwrap();
    assert!(rep.reconstructible, "{rep:?}");
    assert_eq!(rep.pieces_rejected, 0, "refusals are retried, not counted as bad pieces");
    volumes::join(std::slice::from_ref(&out), &l.dir.join("joined.tsr")).unwrap();
    assert_eq!(std::fs::read(l.dir.join("joined.tsr")).unwrap(), l.archive_bytes);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn rate_limited_tls_connections_are_closed_without_a_reply() {
    let l = lab("tlsrate");
    let server_id = tls::generate(&l.dir.join("server.key")).unwrap();
    let client_id = tls::generate(&l.dir.join("client.key")).unwrap();
    let limits = ServerLimits { max_requests_per_second: 2, ..ServerLimits::default() };
    let (addr, stop) = serve(&l.volumes, limits, Some((&server_id, &[client_id.fingerprint])));
    let list = || transfer::list_peer(&addr, Some(&client_id), Some(server_id.fingerprint));
    // a burst of four (twice the rate), then the fifth is closed before any handshake
    for _ in 0..4 {
        assert_eq!(list().unwrap().len(), 1);
    }
    let err = list().unwrap_err().to_string();
    assert!(!err.contains("429"), "no protocol reply is sent in TLS mode: {err}");
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(list().unwrap().len(), 1);
    // and a pinned fetch (descriptor, have list, eight pieces) still completes with the back-off
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, Some(ClientTls { identity: Some(client_id.clone()), peer_ids: vec![server_id.fingerprint] }))).unwrap();
    assert!(rep.reconstructible && rep.encrypted, "{rep:?}");
    assert!(rep.seconds_handshake > 0.0 && rep.requests >= 10, "{rep:?}");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}
