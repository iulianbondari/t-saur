//! `tsaur` — command-line front end. Every command supports `--json`; exit codes are stable
//! (0 ok, 1 generic/I-O, 2 corrupt, 3 policy, 4 limits, 5 missing data, 6 crypto, 7 invalid).
//! `tsaur mcp` serves the same operations to AI agents over the Model Context Protocol.

mod mcp;
mod ops;

use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
use tsaur_core::chunk::ChunkParams;
use tsaur_core::crypto::KdfParams;
use tsaur_core::manifest::Limits;
use tsaur_core::{pack, pieces, tls, transfer, volumes, Error, PackOptions, Reader};

#[derive(Parser)]
#[command(name = "tsaur", version, about = "T-saur (.tsr): content-addressed, agent-first archives")]
struct Cli {
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct PasswordArgs {
    /// Passphrase (prefer --password-env or the TSAUR_PASSWORD variable)
    #[arg(long)]
    password: Option<String>,
    /// Environment variable holding the passphrase
    #[arg(long, default_value = "TSAUR_PASSWORD")]
    password_env: String,
    /// Reference archive(s): chunks found there are referenced instead of stored (pack) or
    /// fetched from there (unpack/verify/read/stat/grep). Repeatable.
    #[arg(long = "ref")]
    refs: Vec<PathBuf>,
    /// Identity file(s) (from `keygen --recipient`) that can unlock hybrid-encrypted archives. Repeatable.
    #[arg(long)]
    identity: Vec<PathBuf>,
}

impl PasswordArgs {
    fn get(&self) -> Option<String> {
        self.password.clone().or_else(|| std::env::var(&self.password_env).ok())
    }

    fn credentials(&self) -> anyhow::Result<tsaur_core::crypto::Credentials> {
        let mut identities = Vec::new();
        for p in &self.identity {
            let bytes = hex::decode(std::fs::read_to_string(p)?.trim())?;
            identities.push(tsaur_core::crypto::hybrid::Identity::from_bytes(&bytes)?);
        }
        Ok(tsaur_core::crypto::Credentials { password: self.get(), identities })
    }

    fn open(&self, archive: &Path) -> anyhow::Result<Reader> {
        Ok(Reader::open_with(archive, &self.credentials()?, &self.refs)?)
    }
}

fn load_recipient(path: &Path) -> anyhow::Result<tsaur_core::crypto::hybrid::Recipient> {
    let bytes = hex::decode(std::fs::read_to_string(path)?.trim())?;
    Ok(tsaur_core::crypto::hybrid::Recipient::from_bytes(&bytes)?)
}

#[derive(Subcommand)]
enum Cmd {
    /// Create an archive from files and directories
    Pack {
        out: PathBuf,
        inputs: Vec<String>,
        /// Compression block size in MiB (consecutive unique chunks compressed together; default 1)
        #[arg(long, default_value_t = 1)]
        block: u32,
        /// Compression block size in KiB (overrides --block for sub-MiB blocks)
        #[arg(long)]
        block_kib: Option<u32>,
        /// Large solid blocks (MiB), best ratio, random access per block (alias for --block N)
        #[arg(long)]
        solid: Option<u32>,
        /// One chunk per blob: best random access, weakest ratio (uses a trained dictionary)
        #[arg(long)]
        granular: bool,
        /// Chunking profile: fine | p2p | archive
        #[arg(long, default_value = "p2p")]
        chunk: String,
        #[arg(long, default_value_t = 19)]
        level: i32,
        /// Codec per block: best (default: zstd, xz and PPMd for text, keep the smallest; x86/ARM64
        /// branch filters tried on machine code), zstd (fast), xz (LZMA2) or ppmd
        #[arg(long, default_value = "best")]
        codec: String,
        /// Codec-choice effort for --codec best: 1 = zstd only, 2 = zstd/xz chosen on a sample of
        /// each block, 3 = zstd/xz/PPMd chosen on a sample (recommended), 4 = 3 plus an xz check when
        /// PPMd wins, 5 = every codec on every block (default). Blocks up to 128 KiB always get the
        /// full trial; other codecs ignore it. Output stays deterministic for a given effort.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=5))]
        effort: u8,
        /// Blocks compressed in parallel (default: CPU threads); lower it to cap memory at high levels
        #[arg(long, default_value_t = 0)]
        jobs: usize,
        #[arg(long)]
        no_dict: bool,
        #[arg(long)]
        no_container: bool,
        /// Do not recompress JPEG files or PDF images with Lepton, so that builds without Lepton
        /// support (the lite build) can read the archive
        #[arg(long)]
        no_lepton: bool,
        /// Disable delta coding against earlier versions (same path in a --ref archive, or a
        /// similarly named earlier entry)
        #[arg(long)]
        no_delta: bool,
        #[command(flatten)]
        pw: PasswordArgs,
        /// Argon2id memory (MiB), min 64
        #[arg(long, default_value_t = 256)]
        kdf_memory_mib: u32,
        /// Ed25519 signing key file (hex seed, see `keygen`)
        #[arg(long)]
        sign_key: Option<PathBuf>,
        /// Encrypt to a hybrid X25519 + ML-KEM-768 recipient (public key file `*.pub` from `keygen --recipient`). Repeatable.
        #[arg(long = "to")]
        to: Vec<PathBuf>,
        #[arg(long)]
        keep_mtime: bool,
        #[arg(long)]
        timestamp: bool,
        /// Also write .pieces / .par sidecars
        #[arg(long)]
        pieces: bool,
        #[arg(long, default_value_t = 64)]
        piece_size_kib: u32,
        /// Parity percentage for the sidecars
        #[arg(long, default_value_t = 10)]
        parity_pct: u32,
        /// Also store canonical views (DOCX -> Markdown, PDF -> text) as entries under .tsaur/views/
        /// (hybrid fidelity: originals stay bit-exact, agents read the views without converting)
        #[arg(long)]
        canonical: bool,
    },
    /// Extract entries
    Unpack {
        archive: PathBuf,
        dir: PathBuf,
        /// Only these entries (exact normalised paths or globs). Repeatable.
        #[arg(long)]
        entry: Vec<String>,
        /// Replace files that already exist in the destination (refused otherwise, before
        /// anything is written)
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// List entries (table, JSON or Markdown agent view)
    List {
        archive: PathBuf,
        #[arg(long)]
        md: bool,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Structure and statistics: sections, codecs, filters, chunks, blobs, references, how the
    /// archive is encrypted or signed (works without credentials for locked archives)
    Info {
        archive: PathBuf,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Verify every entry (and the signature / pieces when requested)
    Verify {
        archive: PathBuf,
        /// Hex Ed25519 public key
        #[arg(long)]
        pubkey: Option<String>,
        #[arg(long)]
        pieces: bool,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Details of one entry: hashes, mode, chunks, container recipe, token estimates
    Stat {
        archive: PathBuf,
        entry: String,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Search a regular expression inside the archive without extracting it
    Grep {
        archive: PathBuf,
        pattern: String,
        /// Restrict to entries matching this glob (e.g. "docs/*.md")
        #[arg(long)]
        entries: Option<String>,
        /// Case-insensitive
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// Maximum number of matches to print
        #[arg(long, default_value_t = 200)]
        max: usize,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Read one entry (whole, a byte range or a line range) to stdout
    Read {
        archive: PathBuf,
        entry: String,
        /// Byte range START-END (END exclusive)
        #[arg(long)]
        bytes: Option<String>,
        /// Line range A-B (1-based, inclusive)
        #[arg(long)]
        lines: Option<String>,
        /// View: raw (default) or canonical (DOCX -> Markdown, PDF -> text; semantic, not bit-exact)
        #[arg(long, default_value = "raw")]
        view: String,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// What changed between two archives: entries added / removed / changed, chunks B shares with A
    Diff {
        a: PathBuf,
        b: PathBuf,
        #[command(flatten)]
        pw: PasswordArgs,
    },
    /// Write .pieces / .par sidecars for an existing archive
    Pieces {
        archive: PathBuf,
        #[arg(long, default_value_t = 64)]
        piece_size_kib: u32,
        #[arg(long, default_value_t = 10)]
        parity_pct: u32,
    },
    /// Repair corrupt pieces from parity
    Recover { archive: PathBuf },
    /// Offline volume sets: split an archive across directories or drives, inspect, join, repair
    Volumes {
        #[command(subcommand)]
        cmd: VolumesCmd,
    },
    /// Generate an Ed25519 signing key (hex seed) and print the public key, or, with --recipient,
    /// a hybrid X25519 + ML-KEM-768 identity (`<out>` secret + `<out>.pub` public recipient key)
    Keygen {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        recipient: bool,
    },
    /// Serve archives to AI agents over the Model Context Protocol (JSON-RPC on stdin/stdout)
    Mcp {
        /// Directory below which archives may be opened and extracted (default: current directory). Repeatable.
        #[arg(long)]
        root: Vec<PathBuf>,
        /// Archive exposed as resources `tsaur://<file name>/<entry>`. Repeatable.
        #[arg(long)]
        archive: Vec<PathBuf>,
        #[command(flatten)]
        pw: PasswordArgs,
    },
}

#[derive(Subcommand)]
enum VolumesCmd {
    /// Split an archive into N data + M parity volumes, placed round-robin over the --out directories
    /// (any N of the N + M volumes rebuild the archive; use one location per volume for real protection)
    Split {
        archive: PathBuf,
        /// Data volumes
        #[arg(long, default_value_t = 4)]
        data: u16,
        /// Parity volumes (how many volumes may be lost)
        #[arg(long, default_value_t = 2)]
        parity: u16,
        /// Piece size in KiB (64 .. 65536); default: adaptive (64 KiB .. 1 MiB, about 16 pieces per data volume)
        #[arg(long)]
        piece_size_kib: Option<u32>,
        /// Output directory; repeatable, volumes are assigned round-robin
        #[arg(long = "out", required = true)]
        outputs: Vec<PathBuf>,
    },
    /// List the volume sets found in the given files or directories; --verify hash-checks every piece
    Inspect {
        paths: Vec<PathBuf>,
        #[arg(long)]
        verify: bool,
    },
    /// Rebuild the archive from any N of its volumes (files or directories)
    Join { out: PathBuf, paths: Vec<PathBuf> },
    /// Recreate missing or damaged volumes from the intact ones (byte-identical to the originals)
    Repair {
        paths: Vec<PathBuf>,
        /// Directory for recreated volumes (default: next to the first intact volume)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Serve the pieces of the given volumes to other T-saur instances (read-only, manual addresses,
    /// no discovery); runs until interrupted
    Serve {
        paths: Vec<PathBuf>,
        /// Address to listen on (loopback by default; other addresses need --expose-lan)
        #[arg(long, default_value = "127.0.0.1:7407")]
        listen: String,
        /// Allow a non-loopback address. It must be combined with who may fetch: --tls-identity
        /// with --allow (encrypted, server authenticated, listed clients only) or --allow-anyone
        #[arg(long)]
        expose_lan: bool,
        /// Concurrent connections accepted in total
        #[arg(long, default_value_t = 64)]
        max_connections: usize,
        /// Concurrent connections accepted per source address
        #[arg(long, default_value_t = 8)]
        max_connections_per_peer: usize,
        /// New connections per second accepted per source address (twice as many in a burst);
        /// one request is one connection, so this is also the piece rate one address can obtain
        #[arg(long, default_value_t = 200)]
        max_requests_per_second: u32,
        /// Slowest client tolerated, in KiB/s: a response must be consumed at least this fast
        /// (after the first 30 s), otherwise the connection is closed and its slot released
        #[arg(long, default_value_t = 64)]
        min_rate_kib: u64,
        /// Identity file made by `volumes keygen`: connections become TLS sessions presenting this
        /// certificate; clients pin its fingerprint with `fetch --peer-id`
        #[arg(long)]
        tls_identity: Option<PathBuf>,
        /// Fingerprint (hex) of a client identity allowed to connect; repeatable; needs --tls-identity
        #[arg(long = "allow")]
        allowed: Vec<String>,
        /// Let ANYONE who can reach the port fetch every served piece (no client authorization).
        /// With --tls-identity the connection is still encrypted and the server still
        /// authenticated; without it nothing is
        #[arg(long)]
        allow_anyone: bool,
    },
    /// Fetch missing volumes of a set from peers (`--from host:port`, repeatable), verifying every piece
    /// against the expected descriptor; interrupted fetches resume; then join offline
    Fetch {
        /// Expected set id (hex, from `split` or `inspect`)
        #[arg(long)]
        set: String,
        /// Expected descriptor hash (hex); without it only the archive hash and geometry are verified
        #[arg(long)]
        descriptor: Option<String>,
        /// Peer address; repeatable
        #[arg(long = "from", required = true)]
        peers: Vec<String>,
        /// Directory that receives the volumes
        #[arg(long)]
        out: PathBuf,
        /// Volumes already present locally (files or directories); repeatable
        #[arg(long)]
        local: Vec<PathBuf>,
        /// What to fetch: needed (default: the fewest volumes that make the set reconstructible), all, or a list like 1,2,5
        #[arg(long, default_value = "needed")]
        volumes: String,
        /// Also rebuild the archive into this file when the set is complete enough
        #[arg(long)]
        join: Option<PathBuf>,
        /// Expected certificate fingerprint (hex) of the corresponding --from peer, in the same
        /// order; when given, every connection is a TLS session pinned to that fingerprint
        #[arg(long = "peer-id")]
        peer_ids: Vec<String>,
        /// Identity file made by `volumes keygen`, presented to peers that require client certificates
        #[arg(long)]
        tls_identity: Option<PathBuf>,
        /// Stop after this many pieces (leaves resumable `.partial` files; for interruption tests)
        #[arg(long)]
        stop_after: Option<usize>,
    },
    /// Create a peer identity for encrypted transfers (private key + self-signed certificate) and
    /// print its fingerprint, which the other side pins
    Keygen {
        /// Identity file to write (the certificate goes next to it as `<out>.crt`); never overwritten
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the fingerprint of an identity file made by `keygen`
    Fingerprint { identity: PathBuf },
}

fn parse_hex32(label: &str, s: &str) -> anyhow::Result<[u8; 32]> {
    let v = hex::decode(s.trim())?;
    let arr: [u8; 32] = v.as_slice().try_into().map_err(|_| anyhow::anyhow!("{label} must be 32 hex bytes"))?;
    Ok(arr)
}

fn run_volumes(cmd: VolumesCmd, json: bool) -> anyhow::Result<i32> {
    match cmd {
        VolumesCmd::Split { archive, data, parity, piece_size_kib, outputs } => {
            let opts = volumes::SplitOptions { data, parity, piece_size: piece_size_kib.map(|k| k.saturating_mul(1024)), outputs };
            let rep = volumes::split(&archive, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!(
                    "set {}  descriptor b3:{}  archive {} bytes b3:{}  pieces {} x {} KiB{}, stripes {}, {} data + {} parity volumes",
                    &rep.set_id[..16],
                    &rep.descriptor_b3[..16],
                    rep.archive_size,
                    &rep.archive_b3[..16],
                    rep.pieces,
                    rep.piece_size / 1024,
                    if rep.piece_size_adaptive { " (adaptive)" } else { "" },
                    rep.stripes,
                    rep.data,
                    rep.parity
                );
                for v in &rep.volumes {
                    println!("  v{:02} {:<6} {:>12} B  {}", v.index + 1, v.kind, v.bytes, v.path.display());
                }
                if rep.placement_warning {
                    println!(
                        "WARNING: {} volumes share one output location but only {} may be lost: losing that location loses the archive. Use one location per volume.",
                        rep.max_per_location, rep.parity
                    );
                }
            }
            Ok(0)
        }
        VolumesCmd::Inspect { paths, verify } => {
            let sets = volumes::inspect(&paths, verify)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&sets)?);
            } else {
                for s in &sets {
                    println!(
                        "set {}  descriptor b3:{}  archive {} ({} bytes, b3:{})  {} data + {} parity, pieces {} x {} KiB, stripes {}",
                        &s.set_id[..16],
                        &s.descriptor_b3[..16],
                        s.archive_name,
                        s.archive_size,
                        &s.archive_b3[..16],
                        s.data,
                        s.parity,
                        s.pieces,
                        s.piece_size / 1024,
                        s.stripes
                    );
                    for (i, v) in s.volumes.iter().enumerate() {
                        match v {
                            Some(f) => println!(
                                "  v{:02} {:<6} {}  header {} descriptor {} size {}{}",
                                i + 1,
                                f.kind,
                                f.path.display(),
                                if f.header_ok { "ok" } else { "BAD" },
                                if f.descriptor_ok { "ok" } else { "BAD" },
                                if f.size_ok { "ok" } else { "BAD" },
                                if f.verified { format!(" pieces bad {}", f.bad_pieces.len()) } else { String::new() }
                            ),
                            None => println!("  v{:02} MISSING ({})", i + 1, s.set.volume_name(i)),
                        }
                    }
                    for (p, why) in &s.extra {
                        println!("  extra: {} ({why})", p.display());
                    }
                    println!(
                        "  reconstructible: {} (stripes short: {}, missing volumes: {}{})",
                        if s.reconstructible { "yes" } else { "NO" },
                        s.stripes_short,
                        s.missing.len(),
                        if verify { "" } else { "; run with --verify to hash-check every piece" }
                    );
                }
            }
            Ok(if sets.iter().all(|s| s.reconstructible) { 0 } else { 5 })
        }
        VolumesCmd::Join { out, paths } => {
            let rep = volumes::join(&paths, &out)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!(
                    "rebuilt {} ({} bytes) from volumes {:?}; missing {:?}; pieces rebuilt {}; archive hash verified: {}",
                    rep.output.display(),
                    rep.archive_size,
                    rep.volumes_used.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    rep.volumes_missing.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    rep.pieces_rebuilt,
                    rep.archive_hash_ok
                );
            }
            Ok(0)
        }
        VolumesCmd::Serve { paths, listen, expose_lan, max_connections, max_connections_per_peer, max_requests_per_second, min_rate_kib, tls_identity, allowed, allow_anyone } => {
            let loopback = transfer::is_loopback(&listen)?;
            // Three separate properties decide what a network client gets: encryption (TLS),
            // the server's identity (its pinned fingerprint) and client authorization (the allow
            // list). Anonymous access is never implied: it has to be asked for by name.
            if !loopback && !expose_lan {
                return Err(Error::Policy(format!(
                    "{listen} is not a loopback address; serving beyond this machine needs --expose-lan together with who may fetch: --tls-identity FILE --allow <fingerprint> ... (recommended) or --allow-anyone"
                ))
                .into());
            }
            if tls_identity.is_none() && !allowed.is_empty() {
                return Err(Error::Invalid("--allow needs --tls-identity: client identities are certificate fingerprints".into()).into());
            }
            if !allowed.is_empty() && allow_anyone {
                return Err(Error::Invalid("--allow and --allow-anyone exclude each other".into()).into());
            }
            if !loopback && allowed.is_empty() && !allow_anyone {
                return Err(Error::Policy(format!(
                    "{listen} is reachable from the network: say who may fetch, either --tls-identity FILE --allow <fingerprint> ... (encrypted, server authenticated, listed clients only) or --allow-anyone (anyone who can reach the port reads every served piece)"
                ))
                .into());
            }
            let (tls_cfg, identity) = match &tls_identity {
                None => (None, None),
                Some(path) => {
                    let id = tls::load(path)?;
                    let allowed_ids = allowed.iter().map(|a| tls::parse_fingerprint(a)).collect::<Result<Vec<_>, _>>()?;
                    if allowed_ids.is_empty() && !allow_anyone {
                        return Err(Error::Policy("--tls-identity needs --allow <fingerprint> (repeatable) or --allow-anyone".into()).into());
                    }
                    (Some(tls::server_config(&id, &allowed_ids)?), Some((id, allowed_ids.len())))
                }
            };
            let limits = transfer::ServerLimits {
                max_connections: max_connections.max(1),
                max_connections_per_peer: max_connections_per_peer.max(1),
                timeout: transfer::TIMEOUT,
                max_requests_per_second: max_requests_per_second.max(1),
                min_rate: min_rate_kib.max(1).saturating_mul(1024),
            };
            let server = transfer::Server::bind_tls(&paths, &listen, limits, tls_cfg)?;
            let fingerprint = identity.as_ref().map(|(id, _)| hex::encode(id.fingerprint));
            let allowed_clients = identity.as_ref().map(|(_, n)| *n).unwrap_or(0);
            let scope = if loopback { "loopback" } else { "lan" };
            let clients = if loopback && identity.is_none() {
                "local"
            } else if allow_anyone {
                "anyone"
            } else {
                "allow-list"
            };
            let access = match (&fingerprint, clients) {
                (None, "local") => "loopback only, plain: processes on this machine".to_string(),
                (Some(fp), "allow-list") => format!("{scope}, encrypted (TLS 1.3), server identity {fp}, clients: {allowed_clients} allowed identit{}", if allowed_clients == 1 { "y" } else { "ies" }),
                (Some(fp), _) => format!("{scope}, encrypted (TLS 1.3), server identity {fp}, clients: ANYONE (no client authorization)"),
                (None, _) => format!("{scope}, UNENCRYPTED, no server identity, clients: ANYONE who can reach this port"),
            };
            if !loopback {
                match (&fingerprint, clients) {
                    (None, _) => {
                        eprintln!("warning: UNENCRYPTED and ANONYMOUS on {listen}: every served piece is readable by anyone who can reach this port; use --tls-identity with --allow to restrict it")
                    }
                    (Some(_), "anyone") => eprintln!(
                        "warning: ANONYMOUS clients on {listen}: the connection is encrypted and clients can verify this server, but anyone who can reach the port can fetch every served piece"
                    ),
                    _ => eprintln!("note: {listen} over TLS; only {allowed_clients} allowed client identit{} can connect", if allowed_clients == 1 { "y" } else { "ies" }),
                }
            }
            let addr = server.local_addr()?;
            let served = server.served();
            // The start-up lines are informational: a parent that stops reading stdout (a closed
            // pipe) must not take the server down, so write errors are ignored here on purpose.
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            if json {
                let access_json = serde_json::json!({"scope": scope, "encrypted": server.encrypted(), "server_identity": fingerprint, "clients": clients, "allowed_clients": allowed_clients});
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&serde_json::json!({"listening": addr.to_string(), "access": access_json, "tls": server.encrypted(), "fingerprint": fingerprint, "sets": served}))?
                );
            } else {
                let _ = writeln!(out, "listening on {addr} ({} set(s)){}", served.len(), if server.encrypted() { "  [tls]" } else { "" });
                let _ = writeln!(out, "  access: {access}");
                if let Some(fp) = &fingerprint {
                    let _ = writeln!(out, "  identity fingerprint {fp}  (clients pin it with --peer-id)");
                }
                for s in &served {
                    let _ = writeln!(out, "  set {}  {} ({} bytes)  volumes {:?}", &s.set_id[..16], s.archive_name, s.archive_size, s.volumes.iter().map(|i| i + 1).collect::<Vec<_>>());
                }
            }
            let _ = out.flush();
            drop(out);
            server.run(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))?;
            Ok(0)
        }
        VolumesCmd::Keygen { out } => {
            if out.exists() || tls::cert_path(&out).exists() {
                return Err(Error::Policy(format!("{} (or its .crt) already exists; keygen never overwrites an identity", out.display())).into());
            }
            let id = tls::generate(&out)?;
            let fp = hex::encode(id.fingerprint);
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({"identity": out.to_string_lossy(), "certificate": tls::cert_path(&out).to_string_lossy(), "fingerprint": fp}))?);
            } else {
                println!("identity     {}  (private key: keep it on this machine)", out.display());
                println!("certificate  {}", tls::cert_path(&out).display());
                println!("fingerprint  {fp}");
                println!("share the fingerprint through a channel you trust; the other side pins it with `serve --allow` or `fetch --peer-id`");
            }
            Ok(0)
        }
        VolumesCmd::Fingerprint { identity } => {
            let id = tls::load(&identity)?;
            let fp = hex::encode(id.fingerprint);
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({"identity": identity.to_string_lossy(), "fingerprint": fp}))?);
            } else {
                println!("{fp}");
            }
            Ok(0)
        }
        VolumesCmd::Fetch { set, descriptor, peers, out, local, volumes: which, join: join_out, peer_ids, tls_identity, stop_after } => {
            let tls_opts = if peer_ids.is_empty() {
                if tls_identity.is_some() {
                    return Err(Error::Invalid("--tls-identity needs one --peer-id per --from peer (the fingerprint printed by that peer's `serve`)".into()).into());
                }
                None
            } else {
                if peer_ids.len() != peers.len() {
                    return Err(Error::Invalid(format!("{} --from peer(s) but {} --peer-id value(s): give one fingerprint per peer, in the same order", peers.len(), peer_ids.len())).into());
                }
                let ids = peer_ids.iter().map(|p| tls::parse_fingerprint(p)).collect::<Result<Vec<_>, _>>()?;
                let identity = match &tls_identity {
                    Some(path) => Some(tls::load(path)?),
                    None => None,
                };
                Some(transfer::ClientTls { identity, peer_ids: ids })
            };
            let want = match which.as_str() {
                "needed" => transfer::Want::Needed,
                "all" => transfer::Want::All,
                list => transfer::Want::Volumes(
                    list.split(',')
                        .map(|x| x.trim().parse::<usize>().map(|i| i.saturating_sub(1)))
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|_| anyhow::anyhow!("--volumes must be needed, all or a list like 1,2,5"))?,
                ),
            };
            let opts = transfer::FetchOptions {
                set_id: parse_hex32("--set", &set)?,
                descriptor_b3: match descriptor {
                    Some(d) => Some(parse_hex32("--descriptor", &d)?),
                    None => None,
                },
                peers,
                out_dir: out.clone(),
                local: local.clone(),
                want,
                max_pieces: stop_after,
                tls: tls_opts,
            };
            let rep = transfer::fetch(&opts)?;
            let mut seconds_join = None;
            let joined = match &join_out {
                Some(path) if rep.reconstructible => {
                    let mut inputs = local.clone();
                    inputs.push(out.clone());
                    let t0 = std::time::Instant::now();
                    let j = volumes::join(&inputs, path)?;
                    seconds_join = Some(t0.elapsed().as_secs_f64());
                    Some(j)
                }
                _ => None,
            };
            if json {
                let mut v = serde_json::to_value(&rep)?;
                if let Some(j) = &joined {
                    v["joined"] = serde_json::to_value(j)?;
                    v["seconds_join"] = serde_json::to_value(seconds_join)?;
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("set {}  descriptor b3:{} (verified against the {}){}", &rep.set_id[..16], &rep.descriptor_b3[..16], rep.verified_against, if rep.encrypted { "  [tls]" } else { "" });
                if rep.verified_against == "set id" {
                    println!("note: without --descriptor the piece hashes are provisional until a join verifies the archive hash");
                }
                println!(
                    "volumes wanted {:?}, completed {:?}, partial {:?}; pieces received {}, rejected {}, re-verified from disk {} (failed {}); {} bytes reserved; peers used {:?}",
                    rep.volumes_wanted.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    rep.volumes_completed.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    rep.volumes_partial.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    rep.pieces_received,
                    rep.pieces_rejected,
                    rep.pieces_reverified,
                    rep.pieces_reverify_failed,
                    rep.bytes_reserved,
                    rep.peers_used
                );
                println!(
                    "{} requests, {} bytes received: connect {:.3} s, tls handshake {:.3} s, transfer {:.3} s, total {:.3} s",
                    rep.requests, rep.bytes_received, rep.seconds_connect, rep.seconds_handshake, rep.seconds_transfer, rep.seconds_total
                );
                println!("reconstructible locally: {}{}", if rep.reconstructible { "yes" } else { "NO" }, if rep.stopped_early { " (stopped early)" } else { "" });
                if let Some(j) = &joined {
                    println!("joined {} ({} bytes) in {:.3} s, archive hash verified: {}", j.output.display(), j.archive_size, seconds_join.unwrap_or(0.0), j.archive_hash_ok);
                }
            }
            Ok(if rep.reconstructible { 0 } else { 5 })
        }
        VolumesCmd::Repair { paths, out } => {
            let rep = volumes::repair(&paths, out.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else if rep.rebuilt.is_empty() {
                println!("all volumes present and intact; nothing to repair");
            } else {
                for v in &rep.rebuilt {
                    println!("  recreated v{:02} {:<6} {:>12} B  {}", v.index + 1, v.kind, v.bytes, v.path.display());
                }
                println!("{} pieces rebuilt from parity", rep.pieces_rebuilt);
            }
            Ok(0)
        }
    }
}

fn expand_inputs(inputs: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for i in inputs {
        if i.contains('*') || i.contains('?') || i.contains('[') {
            let mut any = false;
            for p in glob::glob(i)? {
                out.push(p?);
                any = true;
            }
            if !any {
                anyhow::bail!("no match for {i}");
            }
        } else {
            out.push(PathBuf::from(i));
        }
    }
    Ok(out)
}

fn load_seed(path: &Path) -> anyhow::Result<[u8; 32]> {
    let text = std::fs::read_to_string(path)?;
    let bytes = hex::decode(text.trim())?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow::anyhow!("key file must hold 32 hex bytes"))?;
    Ok(arr)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.cmd {
        Cmd::Pack {
            out,
            inputs,
            no_lepton,
            block,
            block_kib,
            solid,
            granular,
            chunk,
            level,
            codec,
            effort,
            jobs,
            no_dict,
            no_container,
            pw,
            kdf_memory_mib,
            sign_key,
            to,
            keep_mtime,
            timestamp,
            pieces: want_pieces,
            piece_size_kib,
            parity_pct,
            canonical,
            no_delta,
        } => {
            let chunk = ChunkParams::by_name(&chunk).ok_or_else(|| anyhow::anyhow!("unknown chunk profile {chunk}"))?;
            let codec_choice = tsaur_core::codec::CodecChoice::by_name(&codec).ok_or_else(|| anyhow::anyhow!("unknown codec {codec} (best | zstd | xz | ppmd)"))?;
            let block_size = if granular {
                0
            } else if let Some(kib) = block_kib {
                kib.max(1).saturating_mul(1024)
            } else {
                solid.unwrap_or(block).max(1).saturating_mul(1 << 20)
            };
            let opts = PackOptions {
                chunk,
                block_size,
                level,
                codec: codec_choice,
                effort,
                batch: jobs,
                references: pw.refs.clone(),
                recipients: to.iter().map(|p| load_recipient(p)).collect::<anyhow::Result<Vec<_>>>()?,
                train_dict: !no_dict,
                container_aware: !no_container,
                jpeg_recompression: !no_lepton,
                password: pw.get(),
                kdf: KdfParams { m_kib: kdf_memory_mib * 1024, t: 3, p: 4 },
                sign_seed: match sign_key {
                    Some(p) => Some(load_seed(&p)?),
                    None => None,
                },
                keep_mtime,
                timestamp,
                canonical,
                delta: !no_delta,
                profile: if granular {
                    "granular".into()
                } else if solid.is_some() {
                    "solid".into()
                } else {
                    "block".into()
                },
                limits: Limits::default(),
                ..PackOptions::default()
            };
            let paths = expand_inputs(&inputs)?;
            let report = pack(&paths, &out, opts)?;
            let pieces_meta = if want_pieces { Some(pieces::write_sidecars(&out, piece_size_kib * 1024, parity_pct as f64 / 100.0)?) } else { None };
            if cli.json {
                let mut v = serde_json::to_value(&report)?;
                if let Some(m) = &pieces_meta {
                    v["pieces"] = serde_json::json!({"count": m.pieces.len(), "parity": m.parity.len(), "root": hex::encode(&m.root)});
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!(
                    "{} entries, {} bytes -> {} bytes ({:.1}%), chunks {} / unique {}, blobs {}, dict {} B, codecs {:?}, containers exploded {} / fallback {}, views {}, encrypted={}, signed={}\nroot b3:{}",
                    report.entries, report.input_bytes, report.archive_bytes, 100.0 * report.archive_bytes as f64 / report.input_bytes.max(1) as f64,
                    report.chunks_total, report.chunks_unique, report.blobs, report.dict_bytes, report.codec_hist,
                    report.containers_exploded, report.containers_fallback, report.views, report.encrypted, report.signed, report.root
                );
                if report.blocks_sampled > 0 {
                    println!("effort {effort}: codec decided on a sample for {} of {} blobs", report.blocks_sampled, report.blobs);
                }
                if report.skipped_links > 0 {
                    eprintln!("note: {} symbolic link(s) inside the inputs were skipped (links are never followed or recreated)", report.skipped_links);
                }
                if let Some(m) = &pieces_meta {
                    println!("pieces: {} x {} KiB, parity {}, root b3:{}", m.pieces.len(), m.piece_size / 1024, m.parity.len(), hex::encode(&m.root));
                }
            }
            Ok(0)
        }
        Cmd::Unpack { archive, dir, entry, overwrite, pw } => {
            let mut r = pw.open(&archive)?;
            let selected = ops::select_entries(&r, &entry)?;
            let written = r.extract(&dir, selected.as_deref(), overwrite)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>())?);
            } else {
                for p in &written {
                    println!("{}", p.display());
                }
                println!("{} entries extracted, all hashes verified", written.len());
            }
            Ok(0)
        }
        Cmd::List { archive, md, pw } => {
            let r = pw.open(&archive)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&ops::list_json(&r))?);
            } else if md {
                print!("{}", ops::list_markdown(&r, &file_name(&archive)));
            } else {
                let m = &r.manifest;
                println!(
                    "tsaur v{} profile={} fidelity={} chunks={} blobs={} encrypted={} signed={} root=b3:{}",
                    m.tsaur,
                    m.profile,
                    m.fidelity,
                    r.index.chunks.len(),
                    r.index.blobs.len(),
                    r.is_encrypted(),
                    r.signature().is_some(),
                    hex::encode(&m.root)
                );
                for e in &m.entries {
                    println!("{:>12}  {:<4}  {}  {}{}", e.size, e.mode, hex::encode(&e.h[..8]), e.path, e.note.as_ref().map(|n| format!("  ({n})")).unwrap_or_default());
                }
            }
            Ok(0)
        }
        Cmd::Info { archive, pw } => {
            let insp = tsaur_core::read::inspect(&archive)?;
            let v = match pw.open(&archive) {
                Ok(r) => ops::info_json(&r, &archive, &insp),
                Err(e) if insp.encrypted && e.downcast_ref::<Error>().is_some_and(|e| e.exit_code() == 6) => ops::locked_info_json(&archive, &insp, &e.to_string()),
                Err(e) => return Err(e),
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{:<14} {} (tsaur v{}, {} bytes)", "file", v["file"].as_str().unwrap_or(""), v["version"], v["file_bytes"]);
                println!("{:<14} encrypted={} signed={}{}", "security", v["encrypted"], v["signed"], if v["locked"] == true { "  LOCKED (credentials required for more)" } else { "" });
                for s in v["stanzas"].as_array().into_iter().flatten() {
                    println!("{:<14} {} m={} MiB t={} p={}", "  stanza", s["t"].as_str().unwrap_or(""), s["m_kib"].as_u64().unwrap_or(0) / 1024, s["t_cost"], s["p"]);
                }
                if let Some(sig) = v["signature"].as_object() {
                    println!("{:<14} {} key {}", "  signature", sig["alg"].as_str().unwrap_or(""), sig["key"].as_str().unwrap_or(""));
                }
                for s in v["sections"].as_array().into_iter().flatten() {
                    println!("{:<14} {:<12} off {:>12} len {:>12}", "  section", s["type"].as_str().unwrap_or(""), s["offset"], s["length"]);
                }
                if v["locked"] != true {
                    if let Some(req) = v["requires"].as_array().filter(|a| !a.is_empty()) {
                        let missing = v["missing_features"].as_array().map(|a| a.len()).unwrap_or(0);
                        println!(
                            "{:<14} reader features {}{}",
                            "requires",
                            req.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "),
                            if missing > 0 { "  (NOT in this build: JPEG entries cannot be decoded)" } else { "" }
                        );
                    }
                    println!(
                        "{:<14} profile={} fidelity={} solid_block={} chunking={}",
                        "layout",
                        v["profile"].as_str().unwrap_or(""),
                        v["fidelity"].as_str().unwrap_or(""),
                        v["solid_block"],
                        v["chunking"]
                    );
                    println!("{:<14} {} entries, modes {}, input {} bytes -> archive {} bytes ({}%)", "content", v["entries"], v["modes"], v["input_bytes"], v["file_bytes"], v["ratio_pct"]);
                    println!("{:<14} {} total ({} bytes), {} external", "chunks", v["chunks"]["total"], v["chunks"]["bytes"], v["chunks"]["external"]);
                    println!(
                        "{:<14} {} blobs, {} -> {} bytes, codecs {}, filters {}, dictionary {} bytes",
                        "blobs", v["blobs"]["count"], v["blobs"]["uncompressed_bytes"], v["blobs"]["compressed_bytes"], v["blobs"]["codecs"], v["blobs"]["filters"], v["dictionary_bytes"]
                    );
                    for x in v["refs"].as_array().into_iter().flatten() {
                        println!("{:<14} {} ({} chunks, {} bytes) root b3:{}", "  ref", x["hint"].as_str().unwrap_or(""), x["chunks"], x["bytes"], x["root"].as_str().unwrap_or(""));
                    }
                    if v["unresolved_external"].as_u64().unwrap_or(0) > 0 {
                        println!("{:<14} {} external chunks are not resolved by the given --ref archives", "WARNING", v["unresolved_external"]);
                    }
                    println!("{:<14} b3:{}", "root", v["root"].as_str().unwrap_or(""));
                }
            }
            Ok(0)
        }
        Cmd::Verify { archive, pubkey, pieces: check_pieces, pw } => {
            let pk = match pubkey {
                Some(h) => Some(ops::parse_pubkey(&h)?),
                None => None,
            };
            let outcome = ops::verify(&archive, &pw.credentials()?, &pw.refs, pk, check_pieces)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome.json)?);
            } else {
                match (&outcome.report, &outcome.open_error) {
                    (Some(r), _) => {
                        println!(
                            "sections ok; entries {}/{} ok; blobs {}; signature {}{}",
                            r.entries_ok,
                            r.entries_total,
                            r.blobs,
                            r.signature.as_deref().unwrap_or("none"),
                            match r.signature_valid {
                                Some(true) => " (VALID)",
                                Some(false) => " (INVALID)",
                                None => "",
                            }
                        );
                        for b in &r.entries_bad {
                            println!("BAD: {b}");
                        }
                    }
                    (None, Some(e)) => println!("archive does not open: {e}"),
                    _ => {}
                }
                if let Some(m) = outcome.json["missing_features"].as_array() {
                    println!(
                        "this build lacks the reader feature(s) {:?} that the archive needs: use the full build (Cargo feature `lepton`)",
                        m.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()
                    );
                }
                if let Some((b, n, m)) = &outcome.pieces {
                    if b.is_empty() {
                        println!("pieces: {n} ok (parity {m})");
                    } else {
                        println!(
                            "pieces corrupt: {:?} of {n} (parity {m}) -> {}",
                            b,
                            if b.len() <= *m as usize { "recoverable with `tsaur recover`" } else { "NOT recoverable (more damage than parity)" }
                        );
                    }
                }
                println!("{}", if outcome.ok { "OK" } else { "FAILED" });
            }
            Ok(if outcome.ok { 0 } else { 2 })
        }
        Cmd::Stat { archive, entry, pw } => {
            let mut r = pw.open(&archive)?;
            let v = ops::stat_json(&mut r, &entry)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{:<12} {}", "path", v["path"].as_str().unwrap_or(""));
                println!("{:<12} {}", "size", v["size"]);
                println!("{:<12} b3:{}", "hash", v["b3"].as_str().unwrap_or(""));
                println!("{:<12} {} ({} chunks)", "mode", v["mode"].as_str().unwrap_or(""), v["chunks"]);
                println!("{:<12} {} view={}", "type", v["mime"].as_str().unwrap_or(""), v["view"].as_str().unwrap_or("none"));
                println!("{:<12} claude≈{} o200k≈{} (estimates)", "tokens", v["tokens_est"]["claude"], v["tokens_est"]["o200k"]);
                if !v["container"].is_null() {
                    println!("{:<12} {}", "container", v["container"]);
                }
                if let Some(n) = v["note"].as_str() {
                    println!("{:<12} {}", "note", n);
                }
                println!("{:<12} bytes rebuilt and hash-verified", "status");
            }
            Ok(0)
        }
        Cmd::Grep { archive, pattern, entries, ignore_case, max, pw } => {
            let mut r = pw.open(&archive)?;
            let matches = ops::grep(&mut r, &pattern, entries.as_deref(), ignore_case, max)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&ops::grep_json(&matches))?);
            } else {
                for m in &matches {
                    println!("{}:{}: {}", m.path, m.line, m.text);
                }
                eprintln!("{} match(es)", matches.len());
            }
            Ok(0)
        }
        Cmd::Read { archive, entry, bytes, lines, view, pw } => {
            let mut r = pw.open(&archive)?;
            let out = ops::read_entry(&mut r, &entry, bytes.as_deref(), lines.as_deref(), &view)?;
            use std::io::Write;
            std::io::stdout().write_all(&out)?;
            Ok(0)
        }
        Cmd::Diff { a, b, pw } => {
            let (ra, rb) = (pw.open(&a)?, pw.open(&b)?);
            let v = ops::diff_json(&ra, &rb);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("A {} ({} entries)  B {} ({} entries){}", a.display(), v["a"]["entries"], b.display(), v["b"]["entries"], if v["identical"] == true { "  IDENTICAL" } else { "" });
                for (label, key) in [("added", "added"), ("removed", "removed"), ("changed", "changed")] {
                    for e in v[key].as_array().into_iter().flatten() {
                        println!("  {label:<8} {}", e["path"].as_str().unwrap_or(""));
                    }
                }
                println!("  unchanged {}", v["unchanged"]);
                let c = &v["chunks"];
                println!(
                    "chunks of B: {} total, {} already in A ({} bytes), {} new ({} bytes = {}% of B) -> `pack --ref` would store only the new ones",
                    c["b_total"], c["shared_with_a"], c["shared_bytes"], c["new_in_b"], c["new_bytes"], c["new_pct"]
                );
            }
            Ok(0)
        }
        Cmd::Pieces { archive, piece_size_kib, parity_pct } => {
            let m = pieces::write_sidecars(&archive, piece_size_kib * 1024, parity_pct as f64 / 100.0)?;
            if cli.json {
                println!("{}", serde_json::json!({"pieces": m.pieces.len(), "parity": m.parity.len(), "piece_size": m.piece_size, "root": hex::encode(&m.root)}));
            } else {
                println!("pieces: {} x {} KiB, parity pieces: {}, root b3:{}", m.pieces.len(), m.piece_size / 1024, m.parity.len(), hex::encode(&m.root));
            }
            Ok(0)
        }
        Cmd::Volumes { cmd } => run_volumes(cmd, cli.json),
        Cmd::Recover { archive } => {
            let (bad_before, meta) = pieces::verify(&archive)?;
            let m_total: u32 = meta.stripes.iter().map(|s| s.m).sum();
            let (fixed, ok) = pieces::recover(&archive)?;
            if cli.json {
                println!("{}", serde_json::json!({"bad_before": bad_before, "parity": m_total, "repaired": fixed, "ok": ok}));
            } else {
                println!(
                    "corrupt pieces before: {:?} (parity available: {m_total}); repaired: {fixed}; archive {}",
                    bad_before,
                    if ok { "verifies (all pieces match their hashes)" } else { "STILL CORRUPT: more damaged pieces than parity in at least one stripe" }
                );
            }
            Ok(if ok { 0 } else { 2 })
        }
        Cmd::Keygen { out, recipient } => {
            if recipient {
                let id = tsaur_core::crypto::hybrid::Identity::generate()?;
                let pub_path = PathBuf::from(format!("{}.pub", out.display()));
                std::fs::write(&out, hex::encode(id.to_bytes()))?;
                std::fs::write(&pub_path, hex::encode(id.recipient().to_bytes()))?;
                if cli.json {
                    println!("{}", serde_json::json!({"identity_file": out.display().to_string(), "recipient_file": pub_path.display().to_string(), "algorithm": "x25519mlkem768"}));
                } else {
                    println!("hybrid X25519 + ML-KEM-768 identity written to {} (keep secret)\nrecipient public key written to {} (share it; use with `pack --to`)", out.display(), pub_path.display());
                }
                return Ok(0);
            }
            let (seed, pk) = tsaur_core::crypto::sig::keygen()?;
            let pub_path = PathBuf::from(format!("{}.pub", out.display()));
            std::fs::write(&out, hex::encode(seed))?;
            std::fs::write(&pub_path, hex::encode(pk))?;
            if cli.json {
                println!("{}", serde_json::json!({"key_file": out.display().to_string(), "pubkey_file": pub_path.display().to_string(), "pubkey": hex::encode(pk)}));
            } else {
                println!("secret seed written to {}\npublic key: {}", out.display(), hex::encode(pk));
            }
            Ok(0)
        }
        Cmd::Mcp { root, archive, pw } => {
            let cfg = mcp::Config { roots: root, archives: archive, creds: pw.credentials()?, refs: pw.refs.clone() };
            let mut server = mcp::Server::new(cfg)?;
            eprintln!("tsaur mcp: serving on stdio (tools: {}, registered archives: {})", mcp::tool_definitions().len(), server.archive_count());
            server.serve()?;
            Ok(0)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let code = e.downcast_ref::<Error>().map(|e| e.exit_code()).unwrap_or(1);
            if json {
                eprintln!("{}", serde_json::json!({"error": e.to_string(), "code": code}));
            } else {
                eprintln!("error: {e}");
            }
            std::process::exit(code);
        }
    }
}
