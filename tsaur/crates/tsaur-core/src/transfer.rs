//! Direct piece exchange between two T-saur instances (prototype): a tiny request/response
//! protocol over TCP with manually supplied peer addresses. No discovery, no NAT traversal, no
//! tracker, no account. The server only serves bytes of the volumes it was started with; the
//! client verifies every descriptor and every piece against what it expects
//! (`docs/design/VOLUME-TRUST.md`) and writes volumes with the offline format, so `join`,
//! `inspect` and `repair` are unchanged.
//!
//! Wire format ("TSXP/1"), one request per connection, an ASCII line of at most 256 bytes:
//!
//! ```text
//! TSXP/1 SETS                                 -> OK <len>\n + CBOR [[set id hex, archive name, archive size], ...]
//! TSXP/1 DESCRIPTOR <set id hex>              -> OK <len>\n + the CBOR descriptor
//! TSXP/1 HAVE <set id hex>                    -> OK <len>\n + CBOR [volume index, ...] (complete, verified volumes)
//! TSXP/1 PIECE <set id hex> <volume> <stripe> -> OK <len>\n + the piece bytes
//! anything else / unknown / too busy          -> ERR <code> <message>\n
//! ```
//!
//! The transport is plain TCP by default: it authenticates nothing and hides nothing, and
//! integrity comes only from the hashes the receiver already expects. With TLS (`crate::tls`)
//! the same requests run inside TLS 1.3 between two self-signed certificates that each side
//! pins by fingerprint: the client accepts only the server whose certificate hashes to the
//! expected fingerprint, and the server accepts only clients on its allow list (mutual
//! authentication) or, when so configured, any client. No certificate authority, account or
//! service is involved; fingerprints travel through the same trusted channel as the set id
//! (`docs/design/VOLUME-TRUST.md` §6). A mismatch ends the handshake before any request is
//! read; a plain client against a TLS server (or the reverse) fails without a response.
//!
//! # Limits (all explicit, all enforced)
//!
//! | What | Limit | Where |
//! |---|---|---|
//! | request line | 256 bytes, ASCII, newline-terminated | server and client |
//! | descriptor response | `volumes::MAX_DESCRIPTOR` (64 MiB) | client (`request` cap) and `VolumeSet::validate` |
//! | pieces per set | `volumes::MAX_PIECES` | `VolumeSet::validate` |
//! | `HAVE` / `SETS` responses | 4 KiB / 1 MiB | client |
//! | piece response | exactly the piece size (64 KiB .. 64 MiB) | client; the server streams it in 1 MiB steps |
//! | memory | one piece per fetch, one 1 MiB buffer per served connection, plus the descriptor | by construction |
//! | temporary space | the full size of every wanted volume is reserved (`set_len`) before its first piece; `FetchReport::bytes_reserved` | client |
//! | concurrent connections | `ServerLimits::max_connections` (64) in total, `max_connections_per_peer` (8) per source address; excess gets `ERR 503` | server |
//! | request rate | `ServerLimits::max_requests_per_second` (200) new connections per second per source address, twice that in a burst (token bucket); excess gets `ERR 429`; clients back off and retry for about two seconds | server, client |
//! | waiting | 30 s connect / read / write timeouts on both sides | both |
//! | connection budget | the request line (and a TLS handshake) must arrive within `timeout`; a response of L bytes must be consumed within max(`timeout`, L / `min_rate`) (`min_rate` 64 KiB/s); a peer that trickles bytes or stops reading is cut off and its slot released | server |
//! | TLS | the handshake (and the peer's certificate check) completes before any request byte is read; in TLS mode an excess connection is closed without a reply instead of `ERR 503`, so no handshake is spent on it | server |
//! | bandwidth | `ServerLimits::max_bandwidth` (unlimited by default): one token bucket of bytes shared by every connection, paid in slices of at most 1/8 s of the rate; the time a connection spends throttled is credited to its budget, so the cap never triggers the `min_rate` cut-off | server |
//! | distinct addresses | `ServerLimits::max_peers` (64): source addresses with an open connection; a further address gets `ERR 503` (a silent close in TLS mode) until one leaves; an address is a resource key, not an identity | server |
//! | authorization | the handshake admits the union of the global allow list and the per-set lists (`Acl`); a set outside a client's lists is answered like an unknown set (`404`), so a client learns nothing about sets it may not read; a revoked fingerprint (`tls::Revocations`) fails the handshake on either side even when pinned or allowed | server, client |
//! | local file names | derived from the set id when the descriptor's archive name is not a safe single path component (`VolumeSet::volume_name`) | client, repair |
//!
//! Interrupted transfers resume: a volume being fetched lives under `<name>.partial` next to a
//! `.partial.map` listing the pieces believed received. The map is a hint, never a proof: on
//! resume every piece it declares complete is read back and hash-checked before it is trusted,
//! a damaged or truncated partial file loses only the pieces that fail, and an unreadable map
//! means every position of the partial file is checked from disk.

use crate::chunk;
use crate::error::{Error, Result};
use crate::format;
use crate::tls::{self, Revocations};
use crate::volumes::{self, VolumeSet, HEADER_LEN, MAX_DESCRIPTOR};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL: &str = "TSXP/1";
pub const MAX_LINE: usize = 256;
pub const MAX_HAVE_RESPONSE: usize = 4 << 10;
pub const MAX_SETS_RESPONSE: usize = 1 << 20;
pub const TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_CHUNK: usize = 1 << 20;

fn hash32(h: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&h[..32]);
    out
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s).ok()?;
    (v.len() == 32).then(|| hash32(&v))
}

/// True when `listen` names a loopback address (the only default the CLI accepts without an
/// explicit opt-in).
pub fn is_loopback(listen: &str) -> Result<bool> {
    let addr = listen.to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or_else(|| Error::Invalid(format!("listen address {listen}")))?;
    Ok(addr.ip().is_loopback())
}

// ---------------------------------------------------------------- server

struct Served {
    set: VolumeSet,
    descriptor: Vec<u8>,
    /// Paths of complete, verified volumes by index.
    volumes: Vec<Option<PathBuf>>,
}

/// Resource limits of a server; the defaults suit a local network.
#[derive(Clone, Copy, Debug)]
pub struct ServerLimits {
    /// Concurrent connections in total.
    pub max_connections: usize,
    /// Concurrent connections per source address.
    pub max_connections_per_peer: usize,
    /// Per read/write operation; also the whole budget for receiving the request line (and the
    /// TLS handshake before it).
    pub timeout: Duration,
    /// New connections per second per source address, sustained; a burst of twice as many is
    /// admitted (token bucket). One request is one connection, so this is also the piece rate
    /// one address can obtain; a sequential fetch with the adaptive piece sizes stays well below
    /// the default on a gigabit link (1 MiB pieces are about 120 per second).
    pub max_requests_per_second: u32,
    /// Bytes per second a client must at least consume: a response of L bytes has to be read
    /// within max(`timeout`, L / `min_rate`), after which the connection is closed.
    pub min_rate: u64,
    /// Bytes per second the server sends in total, across all connections (`None`: unlimited).
    /// One token bucket holding one second of the rate, shared by every connection in arrival
    /// order; the effective rate of one client is the cap divided by the active connections.
    pub max_bandwidth: Option<u64>,
    /// Distinct source addresses with at least one open connection; a further address is turned
    /// away like an excess connection (`503`, silent in TLS mode) until one of them leaves.
    pub max_peers: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self { max_connections: 64, max_connections_per_peer: 8, timeout: TIMEOUT, max_requests_per_second: 200, min_rate: 64 << 10, max_bandwidth: None, max_peers: 64 }
    }
}

/// Why a connection was not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admit {
    Ok,
    Busy,
    RateLimited,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

struct Gate {
    limits: ServerLimits,
    total: AtomicUsize,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    /// The global bandwidth bucket (tokens are bytes), present when `max_bandwidth` is set.
    throttle: Option<Mutex<Bucket>>,
}

impl Gate {
    fn new(limits: ServerLimits) -> Gate {
        let throttle = limits.max_bandwidth.map(|rate| Mutex::new(Bucket { tokens: rate as f64, last: Instant::now() }));
        Gate { limits, total: AtomicUsize::new(0), per_ip: Mutex::new(HashMap::new()), buckets: Mutex::new(HashMap::new()), throttle }
    }

    fn enter(&self, ip: IpAddr) -> Admit {
        let mut per_ip = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        let mine = per_ip.get(&ip).copied().unwrap_or(0);
        if self.total.load(Ordering::Relaxed) >= self.limits.max_connections || mine >= self.limits.max_connections_per_peer {
            return Admit::Busy;
        }
        // an address that is not present yet needs a free address slot (`per_ip` drops an
        // address at its last `leave`, so "present" is exactly "has an open connection")
        if mine == 0 && per_ip.len() >= self.limits.max_peers {
            return Admit::Busy;
        }
        // token bucket per source address: `max_requests_per_second` sustained, twice that in a burst
        let rate = f64::from(self.limits.max_requests_per_second.max(1));
        let burst = rate * 2.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > 4096 {
            // forget addresses whose bucket would be full again by now (nothing to remember)
            buckets.retain(|_, b| b.tokens + now.duration_since(b.last).as_secs_f64() * rate < burst);
        }
        let b = buckets.entry(ip).or_insert(Bucket { tokens: burst, last: now });
        b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate).min(burst);
        b.last = now;
        if b.tokens < 1.0 {
            return Admit::RateLimited;
        }
        b.tokens -= 1.0;
        per_ip.insert(ip, mine + 1);
        self.total.fetch_add(1, Ordering::Relaxed);
        Admit::Ok
    }

    fn leave(&self, ip: IpAddr) {
        let mut per_ip = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = per_ip.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                per_ip.remove(&ip);
            }
        }
        self.total.fetch_sub(1, Ordering::Relaxed);
    }

    /// Largest slice one write may take from the bandwidth bucket: at most an eighth of a
    /// second of the rate, at least 4 KiB, so that no connection waits long for one grant.
    fn slice_len(&self) -> Option<usize> {
        self.limits.max_bandwidth.map(|rate| ((rate / 8) as usize).max(4 << 10))
    }

    /// Take `n` bytes from the shared bandwidth bucket and return how long the caller has to
    /// wait before sending them (zero when tokens were available). The debt is recorded at once,
    /// so concurrent writers queue up in arrival order; the caller sleeps outside the lock.
    fn take_bytes(&self, n: usize) -> Duration {
        let (Some(bucket), Some(rate)) = (&self.throttle, self.limits.max_bandwidth) else { return Duration::ZERO };
        let rate = rate.max(1) as f64;
        let mut b = bucket.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate).min(rate);
        b.last = now;
        b.tokens -= n as f64;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate)
        }
    }
}

/// Which client may read which set (`docs/design/VOLUME-TRUST.md` §6.4). The handshake admits
/// every listed fingerprint; this decides, per request, whether the admitted client may read the
/// set it asks for. A set outside a client's lists is answered like an unknown set.
#[derive(Clone, Debug, Default)]
pub struct Acl {
    anyone: bool,
    any_set: HashSet<[u8; 32]>,
    per_set: HashMap<[u8; 32], HashSet<[u8; 32]>>,
}

impl Acl {
    /// Every admitted connection may read every set (plain mode, `--allow-anyone`, and a plain
    /// global allow list, where the handshake already decided).
    pub fn everyone() -> Acl {
        Acl { anyone: true, ..Acl::default() }
    }

    /// `any_set`: fingerprints that may read every served set; `per_set`: (set id, fingerprint)
    /// pairs for clients restricted to specific sets.
    pub fn new(any_set: impl IntoIterator<Item = [u8; 32]>, per_set: impl IntoIterator<Item = ([u8; 32], [u8; 32])>) -> Acl {
        let mut acl = Acl { anyone: false, any_set: any_set.into_iter().collect(), per_set: HashMap::new() };
        for (set, fp) in per_set {
            acl.per_set.entry(set).or_default().insert(fp);
        }
        acl
    }

    /// Every fingerprint the handshake must admit (the union of all lists).
    pub fn admitted(&self) -> Vec<[u8; 32]> {
        let mut all: Vec<[u8; 32]> = self.any_set.iter().copied().chain(self.per_set.values().flatten().copied()).collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    /// Fingerprints that may read only some sets (none of them is in the global list).
    pub fn restricted(&self) -> Vec<[u8; 32]> {
        let mut r: Vec<[u8; 32]> = self.per_set.values().flatten().copied().filter(|fp| !self.any_set.contains(fp)).collect();
        r.sort_unstable();
        r.dedup();
        r
    }

    /// How many identities may read `set`: `None` when everyone admitted may.
    pub fn readers_of(&self, set: &[u8; 32]) -> Option<usize> {
        if self.anyone {
            return None;
        }
        Some(self.any_set.len() + self.per_set.get(set).map(|s| s.iter().filter(|fp| !self.any_set.contains(*fp)).count()).unwrap_or(0))
    }

    pub fn may_read(&self, client: Option<&[u8; 32]>, set: &[u8; 32]) -> bool {
        self.anyone || client.is_some_and(|c| self.any_set.contains(c) || self.per_set.get(set).is_some_and(|s| s.contains(c)))
    }
}

/// A read-only server for the volumes found under `paths`.
pub struct Server {
    sets: Arc<HashMap<[u8; 32], Served>>,
    listener: TcpListener,
    gate: Arc<Gate>,
    tls: Option<Arc<ServerConfig>>,
    acl: Arc<Acl>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServedSet {
    pub set_id: String,
    pub archive_name: String,
    pub archive_size: u64,
    pub volumes: Vec<usize>,
}

impl Server {
    /// Inspect and verify the volumes under `paths`, bind `listen` (for example `127.0.0.1:0`).
    pub fn bind(paths: &[PathBuf], listen: &str) -> Result<Server> {
        Self::bind_with(paths, listen, ServerLimits::default())
    }

    pub fn bind_with(paths: &[PathBuf], listen: &str, limits: ServerLimits) -> Result<Server> {
        Self::bind_tls(paths, listen, limits, None)
    }

    /// Like `bind_with`; with `tls` every connection is a TLS session using that configuration
    /// (see `tls::server_config`: the server's own identity and the client allow list). Every
    /// admitted client may read every set.
    pub fn bind_tls(paths: &[PathBuf], listen: &str, limits: ServerLimits, tls: Option<Arc<ServerConfig>>) -> Result<Server> {
        Self::bind_acl(paths, listen, limits, tls, Acl::everyone())
    }

    /// Like `bind_tls` with per-request authorization: `acl` decides which admitted client may
    /// read which set (the TLS configuration must admit `acl.admitted()`; the two are built
    /// together by the CLI). Without `tls` there is no client identity, so `acl` must be
    /// `Acl::everyone()` (anything else would refuse every request).
    pub fn bind_acl(paths: &[PathBuf], listen: &str, limits: ServerLimits, tls: Option<Arc<ServerConfig>>, acl: Acl) -> Result<Server> {
        if tls.is_none() && !acl.anyone {
            return Err(Error::Invalid("per-set client lists need TLS: a plain connection carries no client identity".into()));
        }
        let statuses = volumes::inspect(paths, true)?;
        let mut sets = HashMap::new();
        for st in statuses {
            let mut id = [0u8; 32];
            id.copy_from_slice(&hex::decode(&st.set_id).map_err(|_| Error::Corrupt("set id".into()))?);
            let vols: Vec<Option<PathBuf>> = st.volumes.iter().map(|v| v.as_ref().filter(|f| f.usable() && (f.descriptor_ok || f.header_ok)).map(|f| f.path.clone())).collect();
            let descriptor = format::cbor_encode(&st.set)?;
            sets.insert(id, Served { set: st.set, descriptor, volumes: vols });
        }
        let listener = TcpListener::bind(listen).map_err(|e| Error::Io(std::io::Error::other(format!("bind {listen}: {e}"))))?;
        Ok(Server { sets: Arc::new(sets), listener, gate: Arc::new(Gate::new(limits)), tls, acl: Arc::new(acl) })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// True when connections are TLS sessions.
    pub fn encrypted(&self) -> bool {
        self.tls.is_some()
    }

    pub fn limits(&self) -> ServerLimits {
        self.gate.limits
    }

    pub fn acl(&self) -> &Acl {
        &self.acl
    }

    pub fn served(&self) -> Vec<ServedSet> {
        let mut out: Vec<ServedSet> = self
            .sets
            .iter()
            .map(|(id, s)| ServedSet {
                set_id: hex::encode(id),
                archive_name: s.set.archive_name.clone(),
                archive_size: s.set.archive_size,
                volumes: s.volumes.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i).collect(),
            })
            .collect();
        out.sort_by(|a, b| a.set_id.cmp(&b.set_id));
        out
    }

    /// Accept connections until `stop` is set (checked every 2 ms, so that a sequential client waits
    /// at most that long between one piece and the next); each admitted request runs
    /// on its own thread. Excess connections are answered `ERR 503 busy`, connections above the
    /// per-address rate `ERR 429`, and closed; in TLS mode both are closed without a reply, so
    /// no handshake is spent on them. Every admitted connection runs under a time budget
    /// (`ServerLimits::timeout`, extended per response by `min_rate`), so a peer that trickles
    /// bytes or stops reading releases its slot when the budget ends.
    pub fn run(self, stop: Arc<AtomicBool>) -> Result<()> {
        self.listener.set_nonblocking(true)?;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    let limits = self.gate.limits;
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(limits.timeout));
                    let _ = stream.set_write_timeout(Some(limits.timeout));
                    match self.gate.enter(peer.ip()) {
                        Admit::Ok => {}
                        refused => {
                            if self.tls.is_none() {
                                let _ = match refused {
                                    Admit::Busy => send_err(&mut stream, 503, "busy: too many connections"),
                                    _ => send_err(&mut stream, 429, "too many requests: slow down"),
                                };
                            }
                            continue;
                        }
                    }
                    let sets = self.sets.clone();
                    let gate = self.gate.clone();
                    let tls = self.tls.clone();
                    let acl = self.acl.clone();
                    std::thread::spawn(move || {
                        let mut stream = Deadline::new(stream, limits.timeout, gate.clone());
                        match tls {
                            None => {
                                let _ = handle(&mut stream, &sets, &limits, &acl, None);
                            }
                            Some(cfg) => {
                                if let Ok(conn) = ServerConnection::new(cfg) {
                                    let mut session = StreamOwned::new(conn, stream);
                                    // The handshake (with the client's certificate check) completes before any
                                    // request is read; when it fails the connection is simply closed.
                                    if session.conn.complete_io(&mut session.sock).is_ok() && !session.conn.is_handshaking() {
                                        // the verified client certificate, when the configuration asked for one
                                        let client = session.conn.peer_certificates().and_then(|c| c.first()).map(tls::fingerprint);
                                        let _ = handle(&mut session, &sets, &limits, &acl, client);
                                        session.conn.send_close_notify();
                                        let _ = session.flush();
                                    }
                                }
                            }
                        }
                        gate.leave(peer.ip());
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }
}

/// A TCP stream whose reads and writes must each finish within the per-operation timeout and
/// all of them before an absolute deadline: a peer that trickles bytes or stops reading cannot
/// hold a connection (and its slot) beyond the budget.
struct Deadline {
    inner: TcpStream,
    deadline: Instant,
    op: Duration,
    /// The server's gate, for the shared bandwidth bucket.
    gate: Arc<Gate>,
}

impl Deadline {
    fn new(inner: TcpStream, op: Duration, gate: Arc<Gate>) -> Deadline {
        Deadline { inner, deadline: Instant::now() + op, op, gate }
    }

    fn slice(&self) -> std::io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connection time budget exhausted"));
        }
        Ok(left.min(self.op).max(Duration::from_millis(1)))
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let t = self.slice()?;
        self.inner.set_read_timeout(Some(t))?;
        self.inner.read(buf)
    }
}

impl Write for Deadline {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Every byte the server sends (status lines, payloads, TLS records) passes here. With a
        // bandwidth cap the write is clamped to one slice, paid from the shared bucket, and the
        // time spent waiting is credited to this connection's budget: throttling by the server
        // must never look like a client that reads too slowly.
        let mut n = buf.len();
        if let Some(max) = self.gate.slice_len() {
            n = n.min(max).max(1);
            let wait = self.gate.take_bytes(n);
            if !wait.is_zero() {
                std::thread::sleep(wait);
                self.deadline += wait;
            }
        }
        let t = self.slice()?;
        self.inner.set_write_timeout(Some(t))?;
        self.inner.write(&buf[..n])
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Extends a connection's time budget for a response of a known size (`ServerLimits::min_rate`).
trait Budget {
    fn allow(&mut self, bytes: u64, limits: &ServerLimits);
}

impl Budget for Deadline {
    fn allow(&mut self, bytes: u64, limits: &ServerLimits) {
        let secs = (bytes / limits.min_rate.max(1)).max(limits.timeout.as_secs());
        self.deadline = Instant::now() + Duration::from_secs(secs);
    }
}

impl<C, T: Read + Write + Budget> Budget for StreamOwned<C, T> {
    fn allow(&mut self, bytes: u64, limits: &ServerLimits) {
        self.sock.allow(bytes, limits)
    }
}

/// Read one newline-terminated line of at most `MAX_LINE` bytes of printable ASCII (tab and CR
/// tolerated). A byte outside that set fails at once, without waiting for a newline: a TLS
/// record or any other binary stream sent to a plain endpoint is refused immediately instead
/// of holding the connection until the timeout.
fn read_line_bounded<R: BufRead>(r: &mut R) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(64);
    loop {
        let available = r.fill_buf()?;
        if available.is_empty() {
            return Err(std::io::Error::other("request line too long or unterminated"));
        }
        let mut used = 0;
        let mut complete = false;
        for &b in available {
            used += 1;
            if b == b'\n' {
                complete = true;
                break;
            }
            if !(b == b'\t' || b == b'\r' || (0x20..0x7f).contains(&b)) {
                r.consume(used);
                return Err(std::io::Error::other("request line is not ASCII"));
            }
            buf.push(b);
            if buf.len() >= MAX_LINE {
                r.consume(used);
                return Err(std::io::Error::other("request line too long or unterminated"));
            }
        }
        r.consume(used);
        if complete {
            return String::from_utf8(buf).map_err(|_| std::io::Error::other("request line is not ASCII"));
        }
    }
}

fn send_err<W: Write>(stream: &mut W, code: u16, msg: &str) -> std::io::Result<()> {
    stream.write_all(format!("ERR {code} {msg}\n").as_bytes())?;
    stream.flush()
}

fn send_ok<W: Write>(stream: &mut W, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(format!("OK {}\n", payload.len()).as_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

/// Stream `len` bytes of `file` from `off` without holding the whole piece in memory.
fn send_ok_from_file<W: Write>(stream: &mut W, file: &mut File, off: u64, len: usize) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(off))?;
    stream.write_all(format!("OK {len}\n").as_bytes())?;
    let mut buf = vec![0u8; STREAM_CHUNK.min(len)];
    let mut left = len;
    while left > 0 {
        let n = buf.len().min(left);
        file.read_exact(&mut buf[..n])?;
        stream.write_all(&buf[..n])?;
        left -= n;
    }
    stream.flush()
}

/// Serve one request on an accepted connection (plain or TLS: anything that reads and writes
/// under a time budget). `client` is the verified certificate fingerprint of a mutually
/// authenticated client (`None` for plain and anonymous connections); `acl` decides which sets it
/// may read, and a set it may not read is answered exactly like an unknown one.
fn handle<S: Read + Write + Budget>(stream: &mut S, sets: &HashMap<[u8; 32], Served>, limits: &ServerLimits, acl: &Acl, client: Option<[u8; 32]>) -> std::io::Result<()> {
    let line = {
        let mut reader = BufReader::new(&mut *stream);
        read_line_bounded(&mut reader)
    };
    let line = match line {
        Ok(l) => l,
        Err(e) => return send_err(stream, 400, &e.to_string()),
    };
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.first() != Some(&PROTOCOL) || parts.len() < 2 {
        return send_err(stream, 400, "expected a TSXP/1 request");
    }
    match parts[1] {
        "SETS" => {
            let list: Vec<(String, String, u64)> =
                sets.iter().filter(|(id, _)| acl.may_read(client.as_ref(), id)).map(|(id, s)| (hex::encode(id), s.set.archive_name.clone(), s.set.archive_size)).collect();
            let bytes = format::cbor_encode(&list).map_err(|e| std::io::Error::other(e.to_string()))?;
            stream.allow(bytes.len() as u64, limits);
            send_ok(stream, &bytes)
        }
        "DESCRIPTOR" | "HAVE" | "PIECE" => {
            let Some(id) = parts.get(2).and_then(|h| parse_hex32(h)) else { return send_err(stream, 400, "bad set id") };
            // a set the client may not read is indistinguishable from one that is not served
            let Some(s) = sets.get(&id).filter(|_| acl.may_read(client.as_ref(), &id)) else { return send_err(stream, 404, "unknown set") };
            match parts[1] {
                "DESCRIPTOR" => {
                    stream.allow(s.descriptor.len() as u64, limits);
                    send_ok(stream, &s.descriptor)
                }
                "HAVE" => {
                    let have: Vec<u16> = s.volumes.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i as u16).collect();
                    let bytes = format::cbor_encode(&have).map_err(|e| std::io::Error::other(e.to_string()))?;
                    stream.allow(bytes.len() as u64, limits);
                    send_ok(stream, &bytes)
                }
                _ => {
                    let (Some(vi), Some(st)) = (parts.get(3).and_then(|x| x.parse::<usize>().ok()), parts.get(4).and_then(|x| x.parse::<u64>().ok())) else {
                        return send_err(stream, 400, "bad piece address");
                    };
                    if parts.len() != 5 || vi >= s.set.total() || st >= s.set.volume_pieces(vi) {
                        return send_err(stream, 404, "no such piece");
                    }
                    let Some(path) = &s.volumes[vi] else { return send_err(stream, 404, "volume not available here") };
                    let ps = s.set.piece_size as usize;
                    let mut f = match File::open(path) {
                        Ok(f) => f,
                        Err(e) => return send_err(stream, 500, &e.to_string()),
                    };
                    stream.allow(ps as u64, limits);
                    send_ok_from_file(stream, &mut f, HEADER_LEN as u64 + st * ps as u64, ps)
                }
            }
        }
        _ => send_err(stream, 400, "unknown request"),
    }
}

// ---------------------------------------------------------------- client

fn io_peer(peer: &str, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(e.kind(), format!("{peer}: {e}")))
}

/// A connection that ended before a reply: in TLS mode this is how a server declines a
/// connection above its limits (no protocol reply is sent), so the client treats it like a
/// `429`/`503` and retries with the same bounded back-off.
fn closed_early(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(e.kind(), UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe)
}

/// Time spent by a client, accumulated over its requests (one connection each).
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub requests: usize,
    pub bytes: u64,
    /// TCP connect.
    pub connect: Duration,
    /// TLS handshake (zero in plain mode).
    pub handshake: Duration,
    /// From sending the request line to the last payload byte.
    pub transfer: Duration,
}

/// A parsed reply: a payload, or the server's refusal (`ERR <code> <message>`).
enum Reply {
    Ok(Vec<u8>),
    Refused(u16, String),
}

fn refusal(peer: &str, code: u16, msg: &str) -> Error {
    Error::Missing(format!("{peer}: {code} {msg}"))
}

/// One request; the response payload is refused (never allocated) above `max_len` bytes. With
/// `tls` the connection is a TLS session pinned to the peer's fingerprint (`tls::client_config`);
/// a peer that presents another certificate fails during the handshake, before the request is
/// sent. Connect, handshake and transfer times are added to `timing`.
fn request(peer: &str, tls: Option<&Arc<ClientConfig>>, line: &str, max_len: usize, timing: &mut Timing) -> Result<Reply> {
    let addr = peer.to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or_else(|| Error::Invalid(format!("peer address {peer}")))?;
    let t0 = Instant::now();
    let stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| io_peer(peer, e))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let t1 = Instant::now();
    timing.connect += t1 - t0;
    match tls {
        None => exchange(stream, peer, line, max_len, timing),
        Some(cfg) => {
            let conn = ClientConnection::new(cfg.clone(), tls::server_name()).map_err(|e| Error::Crypto(format!("{peer}: tls: {e}")))?;
            let mut session = StreamOwned::new(conn, stream);
            // finish the handshake (and the pinned-fingerprint check) before the request is sent
            session.conn.complete_io(&mut session.sock).map_err(|e| io_peer(peer, e))?;
            timing.handshake += t1.elapsed();
            exchange(session, peer, line, max_len, timing)
        }
    }
}

fn exchange<S: Read + Write>(mut stream: S, peer: &str, line: &str, max_len: usize, timing: &mut Timing) -> Result<Reply> {
    let t0 = Instant::now();
    stream.write_all(format!("{PROTOCOL} {line}\n").as_bytes()).map_err(|e| io_peer(peer, e))?;
    stream.flush().map_err(|e| io_peer(peer, e))?;
    let mut reader = BufReader::new(stream);
    let status = read_line_bounded(&mut reader).map_err(|e| io_peer(peer, e))?;
    let mut words = status.splitn(3, ' ');
    match (words.next(), words.next(), words.next()) {
        (Some("OK"), Some(len), _) => {
            let len: usize = len.parse().map_err(|_| Error::Corrupt(format!("{peer}: bad response length")))?;
            if len > max_len {
                return Err(Error::Limit(format!("{peer}: response of {len} bytes exceeds the {max_len}-byte limit")));
            }
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).map_err(|e| io_peer(peer, e))?;
            timing.transfer += t0.elapsed();
            timing.requests += 1;
            timing.bytes += len as u64;
            Ok(Reply::Ok(buf))
        }
        (Some("ERR"), code, msg) => Ok(Reply::Refused(code.and_then(|c| c.parse().ok()).unwrap_or(0), msg.unwrap_or("").to_string())),
        _ => Err(Error::Corrupt(format!("{peer}: unexpected response"))),
    }
}

/// TLS settings of a client: `peer_ids[i]` is the expected certificate fingerprint of
/// `FetchOptions::peers[i]` (same length, same order); `identity` is presented to servers that
/// require client certificates and may be `None` for servers that accept anonymous clients;
/// `revoked` fingerprints are refused even when pinned (a pinned peer that is revoked makes the
/// whole fetch fail before any connection: skipping it silently would make a mistake in the
/// wrong file look like an unreachable peer).
#[derive(Clone, Default)]
pub struct ClientTls {
    pub identity: Option<tls::Identity>,
    pub peer_ids: Vec<[u8; 32]>,
    pub revoked: Revocations,
}

/// The per-peer connection settings of one fetch, built once, plus the accumulated timing.
struct Dial {
    configs: HashMap<String, Option<Arc<ClientConfig>>>,
    timing: RefCell<Timing>,
}

impl Dial {
    fn new(peers: &[String], tls: Option<&ClientTls>) -> Result<Dial> {
        if let Some(t) = tls {
            if t.peer_ids.len() != peers.len() {
                return Err(Error::Invalid(format!("{} peer addresses but {} expected peer identities: give one identity per peer, in the same order", peers.len(), t.peer_ids.len())));
            }
        }
        let mut configs = HashMap::new();
        for (i, peer) in peers.iter().enumerate() {
            let cfg = match tls {
                None => None,
                Some(t) => {
                    if t.revoked.contains(&t.peer_ids[i]) {
                        return Err(Error::Policy(format!("peer identity {} ({peer}) is revoked; edit the command instead of skipping the peer", hex::encode(t.peer_ids[i]))));
                    }
                    Some(tls::client_config_with(t.identity.as_ref(), t.peer_ids[i], &t.revoked)?)
                }
            };
            configs.insert(peer.clone(), cfg);
        }
        Ok(Dial { configs, timing: RefCell::new(Timing::default()) })
    }

    /// One request with a bounded back-off when the peer answers `429` (rate) or `503` (busy),
    /// or closes the connection before replying (the TLS-mode form of the same refusals): about
    /// two and a half seconds in total, then the refusal is reported.
    fn request(&self, peer: &str, line: &str, max_len: usize) -> Result<Vec<u8>> {
        let cfg = self.configs.get(peer).and_then(|c| c.as_ref());
        let mut attempt = 0u32;
        loop {
            let outcome = request(peer, cfg, line, max_len, &mut self.timing.borrow_mut());
            let refused = match outcome {
                Ok(Reply::Ok(bytes)) => return Ok(bytes),
                Ok(Reply::Refused(code @ (429 | 503), msg)) => refusal(peer, code, &msg),
                Ok(Reply::Refused(code, msg)) => return Err(refusal(peer, code, &msg)),
                Err(Error::Io(e)) if closed_early(&e) => Error::Io(e),
                Err(e) => return Err(e),
            };
            if attempt >= 16 {
                return Err(refused);
            }
            attempt += 1;
            std::thread::sleep(Duration::from_millis(u64::from(25 * attempt.min(8))));
        }
    }
}

/// Which volumes a fetch should bring to the local side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// The fewest volumes that make the set reconstructible locally (data volumes first).
    Needed,
    /// Every volume of the set.
    All,
    /// Specific volume indices (0-based).
    Volumes(Vec<usize>),
}

pub struct FetchOptions {
    pub set_id: [u8; 32],
    /// When known, the received descriptor must hash to this value; otherwise only the set id
    /// (archive hash + geometry) is verified and piece hashes stay provisional until `join`.
    pub descriptor_b3: Option<[u8; 32]>,
    pub peers: Vec<String>,
    pub out_dir: PathBuf,
    /// Volumes (files or directories) already present locally, besides `out_dir`.
    pub local: Vec<PathBuf>,
    pub want: Want,
    /// Stop after this many pieces (used to exercise interruption and resume).
    pub max_pieces: Option<usize>,
    /// `None`: plain TCP. `Some`: every connection is TLS, pinned per peer (see `ClientTls`).
    pub tls: Option<ClientTls>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FetchReport {
    pub set_id: String,
    pub descriptor_b3: String,
    /// "descriptor" when the descriptor hash was supplied and matched, else "set id".
    pub verified_against: &'static str,
    pub volumes_wanted: Vec<usize>,
    pub volumes_completed: Vec<usize>,
    pub volumes_partial: Vec<usize>,
    pub pieces_received: usize,
    /// Pieces or descriptors from peers that failed verification or arrived malformed.
    pub pieces_rejected: usize,
    /// Pieces of partial files that were re-read on resume and matched their hash.
    pub pieces_reverified: usize,
    /// Pieces the map declared complete whose bytes on disk did not match (fetched again).
    pub pieces_reverify_failed: usize,
    /// Bytes reserved on disk for the wanted volumes (full volume sizes).
    pub bytes_reserved: u64,
    pub peers_used: Vec<String>,
    /// After the fetch, with everything local (existing volumes + out_dir).
    pub reconstructible: bool,
    pub stopped_early: bool,
    /// True when the connections were TLS sessions with pinned peer identities.
    pub encrypted: bool,
    /// Connections opened (one per request) and payload bytes received.
    pub requests: usize,
    pub bytes_received: u64,
    /// Seconds spent in TCP connects, TLS handshakes, request/response transfers, and in total
    /// (including local verification and disk writes).
    pub seconds_connect: f64,
    pub seconds_handshake: f64,
    pub seconds_transfer: f64,
    pub seconds_total: f64,
}

/// Progress map of a volume being fetched, stored next to the `.partial` file (a hint only).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PartialMap {
    descriptor_b3: ByteBuf,
    index: u16,
    have: Vec<bool>,
}

fn partial_paths(out_dir: &Path, set: &VolumeSet, vi: usize) -> (PathBuf, PathBuf, PathBuf) {
    let final_path = out_dir.join(set.volume_name(vi));
    let partial = out_dir.join(format!("{}.partial", set.volume_name(vi)));
    let map = out_dir.join(format!("{}.partial.map", set.volume_name(vi)));
    (final_path, partial, map)
}

/// Write the map atomically (temporary file + rename) so that an interruption leaves the old map.
fn save_map(map_path: &Path, map: &PartialMap) -> Result<()> {
    let tmp = map_path.with_extension("map.tmp");
    std::fs::write(&tmp, format::cbor_encode(map)?)?;
    if map_path.exists() {
        std::fs::remove_file(map_path)?;
    }
    std::fs::rename(&tmp, map_path)?;
    Ok(())
}

fn expected_piece_hash(set: &VolumeSet, vi: usize, s: usize) -> [u8; 32] {
    let n = set.data as usize;
    if set.is_parity(vi) {
        set.parity_hash(s as u32, vi - n)
    } else {
        set.piece_hash((s as u64 * n as u64 + vi as u64) as u32)
    }
}

/// Read piece `s` of the partial file and compare it with the descriptor hash.
fn piece_on_disk_ok(file: &mut File, set: &VolumeSet, vi: usize, s: usize, buf: &mut [u8]) -> bool {
    let ps = set.piece_size as usize;
    if file.seek(SeekFrom::Start(HEADER_LEN as u64 + s as u64 * ps as u64)).is_err() {
        return false;
    }
    let mut got = 0;
    while got < ps {
        match file.read(&mut buf[got..]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => got += n,
        }
    }
    chunk::hash(&buf[..ps]) == expected_piece_hash(set, vi, s)
}

/// Obtain and verify the descriptor: from a local volume when one exists, else from the peers.
fn obtain_descriptor(opts: &FetchOptions, dial: &Dial, peers_used: &mut Vec<String>) -> Result<(VolumeSet, [u8; 32], &'static str)> {
    let mut local_paths = opts.local.clone();
    if opts.out_dir.is_dir() {
        local_paths.push(opts.out_dir.clone());
    }
    if !local_paths.is_empty() {
        if let Ok(sets) = volumes::inspect(&local_paths, false) {
            for st in sets {
                if st.set_id == hex::encode(opts.set_id) {
                    let bytes = format::cbor_encode(&st.set)?;
                    let h = chunk::hash(&bytes);
                    if let Some(expected) = opts.descriptor_b3 {
                        if h != expected {
                            return Err(Error::Corrupt("the local volumes carry a descriptor that does not match the expected descriptor hash".into()));
                        }
                    }
                    return Ok((st.set, h, if opts.descriptor_b3.is_some() { "descriptor" } else { "set id" }));
                }
            }
        }
    }
    let mut last_err = None;
    for peer in &opts.peers {
        match dial.request(peer, &format!("DESCRIPTOR {}", hex::encode(opts.set_id)), MAX_DESCRIPTOR as usize) {
            Ok(bytes) => match verify_descriptor(&bytes, opts) {
                Ok((set, h, how)) => {
                    peers_used.push(peer.clone());
                    return Ok((set, h, how));
                }
                Err(e) => last_err = Some(format!("{peer}: {e}")),
            },
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(Error::Missing(format!("no peer supplied an acceptable descriptor ({})", last_err.unwrap_or_else(|| "no peers".into()))))
}

fn verify_descriptor(bytes: &[u8], opts: &FetchOptions) -> Result<(VolumeSet, [u8; 32], &'static str)> {
    let h = chunk::hash(bytes);
    if let Some(expected) = opts.descriptor_b3 {
        if h != expected {
            return Err(Error::HashMismatch("descriptor hash".into()));
        }
    }
    let set: VolumeSet = format::cbor_decode(bytes)?;
    set.validate()?;
    let recomputed = volumes::set_id_for(&hash32(&set.archive_b3), set.archive_size, set.piece_size, set.data, set.parity);
    if recomputed != opts.set_id || set.set_id[..] != opts.set_id[..] {
        return Err(Error::HashMismatch("descriptor does not belong to the expected set id".into()));
    }
    Ok((set, h, if opts.descriptor_b3.is_some() { "descriptor" } else { "set id" }))
}

/// Fetch the wanted volumes from the peers into `out_dir`, verifying everything on the way.
pub fn fetch(opts: &FetchOptions) -> Result<FetchReport> {
    if opts.peers.is_empty() {
        return Err(Error::Invalid("at least one peer address is required".into()));
    }
    let started = Instant::now();
    let dial = Dial::new(&opts.peers, opts.tls.as_ref())?;
    std::fs::create_dir_all(&opts.out_dir)?;
    let mut peers_used: Vec<String> = Vec::new();
    let (set, descriptor_b3, verified_against) = obtain_descriptor(opts, &dial, &mut peers_used)?;
    let descriptor_bytes = format::cbor_encode(&set)?;

    // what is complete locally already
    let mut local_paths = opts.local.clone();
    local_paths.push(opts.out_dir.clone());
    let local_complete: Vec<usize> = match volumes::inspect(&local_paths, true) {
        Ok(sets) => sets
            .into_iter()
            .find(|s| s.set_id == hex::encode(opts.set_id))
            .map(|s| s.volumes.iter().enumerate().filter(|(_, v)| v.as_ref().is_some_and(|f| f.usable())).map(|(i, _)| i).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // what every peer has
    let mut availability: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for peer in &opts.peers {
        if let Ok(bytes) = dial.request(peer, &format!("HAVE {}", hex::encode(opts.set_id)), MAX_HAVE_RESPONSE) {
            if let Ok(have) = format::cbor_decode::<Vec<u16>>(&bytes) {
                for vi in have {
                    if (vi as usize) < set.total() {
                        availability.entry(vi as usize).or_default().push(peer.clone());
                    }
                }
            }
        }
    }

    let n = set.data as usize;
    let wanted: Vec<usize> = match &opts.want {
        Want::All => (0..set.total()).filter(|vi| !local_complete.contains(vi)).collect(),
        Want::Volumes(list) => list.iter().copied().filter(|vi| *vi < set.total() && !local_complete.contains(vi)).collect(),
        Want::Needed => {
            let mut chosen = Vec::new();
            let mut have = local_complete.len();
            for vi in (0..n).chain(n..set.total()) {
                if have >= n {
                    break;
                }
                if !local_complete.contains(&vi) && availability.contains_key(&vi) {
                    chosen.push(vi);
                    have += 1;
                }
            }
            if have < n {
                return Err(Error::Missing(format!(
                    "the peers hold only {} of the {n} volumes needed (local {}, available remotely {:?})",
                    have,
                    local_complete.len(),
                    availability.keys().collect::<Vec<_>>()
                )));
            }
            chosen
        }
    };

    let ps = set.piece_size as usize;
    let mut buf = vec![0u8; ps];
    let mut received = 0usize;
    let mut rejected = 0usize;
    let mut reverified = 0usize;
    let mut reverify_failed = 0usize;
    let mut bytes_reserved = 0u64;
    let mut completed = Vec::new();
    let mut partial = Vec::new();
    let mut stopped_early = false;
    'volumes: for &vi in &wanted {
        let (final_path, partial_path, map_path) = partial_paths(&opts.out_dir, &set, vi);
        let count = set.volume_pieces(vi) as usize;
        let full_len = HEADER_LEN as u64 + set.payload_len(vi);
        // resume state: the map is a hint; every piece it claims is read back and hash-checked
        let hint: Option<Vec<bool>> = match std::fs::read(&map_path).ok().and_then(|b| format::cbor_decode::<PartialMap>(&b).ok()) {
            Some(m) if m.descriptor_b3[..] == descriptor_b3[..] && m.index as usize == vi && m.have.len() == count => Some(m.have),
            Some(_) => None, // a map of another set or geometry: trust nothing it says
            None => None,    // missing or damaged map: check every position of the partial file
        };
        let mut have = vec![false; count];
        if partial_path.is_file() {
            let mut f = OpenOptions::new().read(true).write(true).open(&partial_path)?;
            let mut header = [0u8; HEADER_LEN];
            let header_ok =
                f.read_exact(&mut header).is_ok() && volumes::decode_header(&header).is_ok_and(|h| h.index as usize == vi && h.set_id[..] == set.set_id[..] && h.piece_size == set.piece_size);
            if header_ok {
                for (s, slot) in have.iter_mut().enumerate() {
                    let claimed = hint.as_ref().is_none_or(|h| h[s]);
                    if !claimed {
                        continue;
                    }
                    if piece_on_disk_ok(&mut f, &set, vi, s, &mut buf) {
                        *slot = true;
                        reverified += 1;
                    } else if hint.is_some() {
                        reverify_failed += 1;
                    }
                }
                if f.metadata()?.len() != full_len {
                    f.set_len(full_len)?;
                }
            } else {
                drop(f);
                std::fs::remove_file(&partial_path)?;
            }
        }
        if !partial_path.is_file() {
            let mut f = File::create(&partial_path)?;
            let mut id = [0u8; 32];
            id.copy_from_slice(&set.set_id);
            f.write_all(&volumes::encode_header(&volumes::VolumeHeader {
                index: vi as u16,
                data: set.data,
                parity: set.parity,
                piece_size: set.piece_size,
                archive_size: set.archive_size,
                set_id: id,
                payload_len: set.payload_len(vi),
            }))?;
            f.set_len(full_len)?;
        }
        bytes_reserved += full_len;
        let mut map = PartialMap { descriptor_b3: ByteBuf::from(descriptor_b3.to_vec()), index: vi as u16, have };
        save_map(&map_path, &map)?;
        let mut file = OpenOptions::new().read(true).write(true).open(&partial_path)?;
        let sources: Vec<String> = availability.get(&vi).cloned().unwrap_or_default();
        if sources.is_empty() && !map.have.iter().all(|&h| h) {
            partial.push(vi);
            continue;
        }
        let mut next_source = 0usize;
        for s in 0..count {
            if map.have[s] {
                continue;
            }
            if opts.max_pieces.is_some_and(|max| received >= max) {
                stopped_early = true;
                save_map(&map_path, &map)?;
                partial.push(vi);
                break 'volumes;
            }
            let expected = expected_piece_hash(&set, vi, s);
            let mut got = None;
            for attempt in 0..sources.len() {
                let peer = &sources[(next_source + attempt) % sources.len()];
                match dial.request(peer, &format!("PIECE {} {vi} {s}", hex::encode(opts.set_id)), ps) {
                    Ok(bytes) if bytes.len() == ps && chunk::hash(&bytes) == expected => {
                        got = Some(bytes);
                        if !peers_used.contains(peer) {
                            peers_used.push(peer.clone());
                        }
                        next_source = (next_source + attempt) % sources.len();
                        break;
                    }
                    Ok(_) => rejected += 1,
                    Err(_) => rejected += 1,
                }
            }
            let Some(bytes) = got else {
                save_map(&map_path, &map)?;
                return Err(Error::Missing(format!("piece {s} of {} could not be obtained from any peer", set.volume_name(vi))));
            };
            file.seek(SeekFrom::Start(HEADER_LEN as u64 + s as u64 * ps as u64))?;
            file.write_all(&bytes)?;
            file.flush()?;
            map.have[s] = true;
            received += 1;
            save_map(&map_path, &map)?;
        }
        if map.have.iter().all(|&h| h) {
            file.seek(SeekFrom::Start(HEADER_LEN as u64 + set.payload_len(vi)))?;
            file.write_all(&descriptor_bytes)?;
            file.write_all(&volumes::encode_trailer(HEADER_LEN as u64 + set.payload_len(vi), descriptor_bytes.len() as u64, &descriptor_b3, vi as u16))?;
            file.flush()?;
            drop(file);
            if final_path.exists() {
                std::fs::remove_file(&final_path)?;
            }
            std::fs::rename(&partial_path, &final_path)?;
            let _ = std::fs::remove_file(&map_path);
            completed.push(vi);
        }
    }
    let reconstructible = volumes::inspect(&local_paths, true).ok().and_then(|sets| sets.into_iter().find(|s| s.set_id == hex::encode(opts.set_id))).is_some_and(|s| s.reconstructible);
    let timing = *dial.timing.borrow();
    Ok(FetchReport {
        set_id: hex::encode(opts.set_id),
        descriptor_b3: hex::encode(descriptor_b3),
        verified_against,
        volumes_wanted: wanted,
        volumes_completed: completed,
        volumes_partial: partial,
        pieces_received: received,
        pieces_rejected: rejected,
        pieces_reverified: reverified,
        pieces_reverify_failed: reverify_failed,
        bytes_reserved,
        peers_used,
        reconstructible,
        stopped_early,
        encrypted: opts.tls.is_some(),
        requests: timing.requests,
        bytes_received: timing.bytes,
        seconds_connect: timing.connect.as_secs_f64(),
        seconds_handshake: timing.handshake.as_secs_f64(),
        seconds_transfer: timing.transfer.as_secs_f64(),
        seconds_total: started.elapsed().as_secs_f64(),
    })
}

/// List the sets a peer serves; with `peer_id` the connection is TLS pinned to that
/// fingerprint, presenting `identity` when given.
pub fn list_peer(peer: &str, identity: Option<&tls::Identity>, peer_id: Option<[u8; 32]>) -> Result<Vec<(String, String, u64)>> {
    list_peer_with(peer, identity, peer_id, &Revocations::default())
}

/// `list_peer` with a revocation list (`ClientTls::revoked`): a revoked peer is refused before
/// any connection, and would fail the handshake anyway.
pub fn list_peer_with(peer: &str, identity: Option<&tls::Identity>, peer_id: Option<[u8; 32]>, revoked: &Revocations) -> Result<Vec<(String, String, u64)>> {
    let cfg = match peer_id {
        Some(id) if revoked.contains(&id) => return Err(Error::Policy(format!("peer identity {} ({peer}) is revoked", hex::encode(id)))),
        Some(id) => Some(tls::client_config_with(identity, id, revoked)?),
        None => None,
    };
    match request(peer, cfg.as_ref(), "SETS", MAX_SETS_RESPONSE, &mut Timing::default())? {
        Reply::Ok(bytes) => format::cbor_decode(&bytes),
        Reply::Refused(code, msg) => Err(refusal(peer, code, &msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    #[test]
    fn gate_refuses_a_new_address_beyond_max_peers() {
        let g = Gate::new(ServerLimits { max_peers: 2, ..ServerLimits::default() });
        assert_eq!(g.enter(ip(1)), Admit::Ok);
        assert_eq!(g.enter(ip(2)), Admit::Ok);
        assert_eq!(g.enter(ip(3)), Admit::Busy, "a third address needs a free address slot");
        assert_eq!(g.enter(ip(1)), Admit::Ok, "a known address is not a new one");
        g.leave(ip(2));
        assert_eq!(g.enter(ip(3)), Admit::Ok, "the slot of an address that left is free again");
        g.leave(ip(1));
        g.leave(ip(1));
        g.leave(ip(3));
        assert_eq!(g.total.load(Ordering::Relaxed), 0);
        assert!(g.per_ip.lock().unwrap().is_empty());
    }

    #[test]
    fn bandwidth_bucket_grants_a_burst_then_meters_the_rate() {
        let g = Gate::new(ServerLimits { max_bandwidth: Some(1 << 20), ..ServerLimits::default() });
        assert_eq!(g.slice_len(), Some(128 << 10));
        // one second of the rate is free (the bucket starts full) ...
        assert!(g.take_bytes(1 << 20).is_zero());
        // ... the next megabyte has to wait about a second, and the debt accumulates in order
        let w1 = g.take_bytes(1 << 20);
        let w2 = g.take_bytes(1 << 20);
        assert!(w1 > Duration::from_millis(900) && w1 <= Duration::from_millis(1100), "{w1:?}");
        assert!(w2 > Duration::from_millis(1900) && w2 <= Duration::from_millis(2100), "{w2:?}");
        // without a cap nothing waits and nothing is clamped
        let free = Gate::new(ServerLimits::default());
        assert_eq!(free.slice_len(), None);
        assert!(free.take_bytes(usize::MAX / 2).is_zero());
        // the smallest slice is 4 KiB whatever the rate
        assert_eq!(Gate::new(ServerLimits { max_bandwidth: Some(1024), ..ServerLimits::default() }).slice_len(), Some(4096));
    }
}
