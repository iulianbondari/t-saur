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
use tsaur_core::{pack, Error, PackOptions};

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
    // twelve pinned requests in a row at 2 per second (burst 4): the first ones are admitted, the
    // rest are closed before any handshake. The bound is clock-aware: a slow machine refills
    // tokens while the burst runs, so the refusal is only required when the burst was quick.
    let t0 = Instant::now();
    let (mut ok, mut refused, mut refusal) = (0, 0, None);
    for _ in 0..12 {
        match list() {
            Ok(sets) => {
                assert_eq!(sets.len(), 1);
                ok += 1;
            }
            Err(e) => {
                refused += 1;
                refusal = Some(e);
            }
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    assert!(ok >= 2, "the burst is admitted: ok {ok}, refused {refused}, {secs:.2} s");
    if secs < 3.0 {
        assert!(refused >= 1, "twelve quick requests at 2 per second must hit the limit: ok {ok}, refused {refused}, {secs:.2} s");
        // A silent close surfaces as an I/O error; a protocol refusal would be Error::Missing
        // ("<peer>: 429 ..."). The variant is checked, not the text: the text carries the peer
        // address, and an ephemeral port such as 44291 contains "429" by itself.
        let e = refusal.as_ref().unwrap();
        assert!(matches!(e, Error::Io(_)), "no protocol reply is sent in TLS mode: {e}");
    }
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(list().unwrap().len(), 1);
    // and a pinned fetch (descriptor, have list, eight pieces) still completes with the back-off
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, Some(ClientTls { identity: Some(client_id.clone()), peer_ids: vec![server_id.fingerprint], ..Default::default() }))).unwrap();
    assert!(rep.reconstructible && rep.encrypted, "{rep:?}");
    assert!(rep.seconds_handshake > 0.0 && rep.requests >= 10, "{rep:?}");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

/// A lab whose volumes have one 256 KiB piece each (2 + 1 over about 500 KB).
fn lab_big_pieces(tag: &str) -> Lab {
    let mut l = lab(tag);
    let vol_dir = l.dir.join("big-volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let rep = volumes::split(&l.dir.join("set.tsr"), &SplitOptions { data: 2, parity: 1, piece_size: Some(262144), outputs: vec![vol_dir] }).unwrap();
    l.volumes = rep.volumes.iter().map(|v| v.path.clone()).collect();
    l.set_id.copy_from_slice(&hex::decode(&rep.set_id).unwrap());
    l.descriptor_b3.copy_from_slice(&hex::decode(&rep.descriptor_b3).unwrap());
    l
}

#[test]
fn bandwidth_cap_bounds_the_transfer_rate() {
    let l = lab("bandwidth");
    let cap: u64 = 256 << 10;
    let (addr, stop) = serve(&l.volumes, ServerLimits { max_bandwidth: Some(cap), ..ServerLimits::default() }, None);
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, None)).unwrap();
    assert!(rep.reconstructible, "{rep:?}");
    // one second of the rate is granted at once (the bucket starts full); everything beyond it
    // is metered, so the fetch cannot have been faster than the cap allows (lower bound only:
    // a slow runner may take longer, never less)
    let metered = rep.bytes_received.saturating_sub(cap) as f64 / cap as f64;
    assert!(rep.seconds_total >= metered * 0.9, "received {} bytes in {:.2} s at a cap of {cap} B/s", rep.bytes_received, rep.seconds_total);
    assert!(rep.bytes_received > cap, "the lab must be larger than one second of the cap: {}", rep.bytes_received);
    volumes::join(std::slice::from_ref(&out), &l.dir.join("joined.tsr")).unwrap();
    assert_eq!(std::fs::read(l.dir.join("joined.tsr")).unwrap(), l.archive_bytes);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn throttled_clients_are_not_cut_off_by_the_time_budget() {
    // budget per response: max(timeout 1 s, 256 KiB / 1 MiB/s) = 1 s; at 128 KiB/s a piece takes
    // about 2 s, so without the throttling credit the connection would be cut after ~128 KiB
    let l = lab_big_pieces("credit");
    let limits = ServerLimits { timeout: Duration::from_secs(1), min_rate: 1 << 20, max_bandwidth: Some(128 << 10), ..ServerLimits::default() };
    let (addr, stop) = serve(&l.volumes, limits, None);
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, None)).unwrap();
    assert!(rep.reconstructible && rep.pieces_rejected == 0, "{rep:?}");
    assert!(rep.seconds_total >= 2.0, "two 256 KiB pieces at 128 KiB/s take at least about three seconds minus the free burst: {:.2} s", rep.seconds_total);
    volumes::join(std::slice::from_ref(&out), &l.dir.join("joined.tsr")).unwrap();
    assert_eq!(std::fs::read(l.dir.join("joined.tsr")).unwrap(), l.archive_bytes);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn bandwidth_cap_is_shared_between_connections() {
    let l = lab("shared");
    let cap: u64 = 256 << 10;
    let (addr, stop) = serve(&l.volumes, ServerLimits { max_bandwidth: Some(cap), ..ServerLimits::default() }, None);
    let t0 = Instant::now();
    let fetches: Vec<_> = (0..2)
        .map(|i| {
            let o = opts(&l, &addr, &l.dir.join(format!("here-{i}")), None);
            std::thread::spawn(move || transfer::fetch(&o).unwrap())
        })
        .collect();
    let reports: Vec<_> = fetches.into_iter().map(|h| h.join().unwrap()).collect();
    let elapsed = t0.elapsed().as_secs_f64();
    let bytes: u64 = reports.iter().map(|r| r.bytes_received).sum();
    assert!(reports.iter().all(|r| r.reconstructible), "{reports:?}");
    // the two fetches together never exceed the cap plus the free burst (loose upper bound)
    assert!(bytes as f64 <= cap as f64 * elapsed * 1.5 + cap as f64 * 2.0, "{bytes} bytes in {elapsed:.2} s at a shared cap of {cap} B/s");
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}

#[test]
fn max_peers_one_still_serves_one_address_fully() {
    let l = lab("peers");
    let (addr, stop) = serve(&l.volumes, ServerLimits { max_peers: 1, ..ServerLimits::default() }, None);
    let out = l.dir.join("here");
    let rep = transfer::fetch(&opts(&l, &addr, &out, None)).unwrap();
    assert!(rep.reconstructible && rep.pieces_rejected == 0, "{rep:?}");
    assert!(raw(&addr, "TSXP/1 SETS\n").unwrap().starts_with("OK "));
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&l.dir);
}
