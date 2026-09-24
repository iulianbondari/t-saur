# Design note: offline volume sets

Status: implemented in `tsaur-core::volumes` and the `tsaur volumes` command group (this note was
written before the implementation and updated with the measured numbers). Measured on the default
archive of benchmark corpus A (956,731 bytes, 4 data + 2 parity volumes, 256 KiB pieces, one
location per volume): six volumes totalling 1,576,464 bytes (165 % of the archive: 50 % parity plus
piece padding on a small archive), split in 0.03 s; with two of the six drives removed,
`inspect --verify` reports the missing volumes and `join` rebuilds the archive from the other four
in 0.02 s, hash-verified and bit-identical (`benchmarks/RESULTS-rust.md`). Tests: any 2 of 6
volumes missing (all 15 combinations), damaged pieces as erasures, damaged header or trailer,
byte-identical repair, no-parity striping, mixed sets, 60 rounds of random damage without a wrong
byte ever being written (`tsaur/crates/tsaur-core/tests/volumes.rs`, `tsaur/crates/tsaur-cli/tests/cli.rs`).

## 1. Goal and non-goals

A user must be able to split one `.tsr` archive across several local directories or removable
drives, inspect and verify what is on each of them, rebuild the archive when at least the required
number of volumes is available, and extract the original files bit-exact. Everything happens
offline: no account, key server, cloud service, blockchain or T-saur-operated server is involved.

Not in this work package: networking, peer discovery, DHTs, blockchain anchoring, a new
compression codec. Direct peer-to-peer exchange of pieces is the next stage, and it will be
built on the same piece identities.

## 2. Format compatibility

The ordinary single-file archive is untouched: a volume set is a *transport form* of an existing
archive file. `split` reads the archive as opaque bytes and `join` writes back a byte-identical
file, so every existing archive (encrypted, signed, with references, any codec) can be split,
and every joined archive is verified with the normal reader. Nothing inside the `.tsr` format
changes; older readers keep opening single-file archives, and the volumes carry a separate
magic (`TSV\x1A`) so they are never mistaken for archives.

The existing `.pieces`/`.par` sidecars (transport pieces with Reed-Solomon parity in one file)
stay as they are. Volume sets reuse the same building blocks — BLAKE3 piece hashes, a Merkle
root over the pieces, systematic Reed-Solomon over GF(2^8) from the same library, CBOR
descriptors — but lay the pieces out *volume-major* so that whole devices can be lost.

## 3. Layout

An archive of `S` bytes is cut into `P = ceil(S / piece_size)` pieces (default 1 MiB, minimum
64 KiB, maximum 64 MiB; the last piece is zero-padded for hashing and parity). With `N` data
volumes and `M` parity volumes:

* data piece `i` lives on data volume `i mod N` at stripe position `i div N`;
* stripe `s` consists of data pieces `s*N .. s*N+N-1` (pieces beyond the archive end are
  virtual all-zero pieces that are never stored) plus one parity piece on each parity volume;
* any `N` intact pieces of a stripe (data or parity) rebuild the stripe, so **any `N` of the
  `N + M` volumes rebuild the archive**, and per stripe up to `M` damaged or missing pieces are
  tolerated wherever they are.

Constraints: `N >= 1`, `M >= 0`, `N + M <= 256` (GF(2^8)), `P <= 2^32 - 1`.

Each volume file (`<archive file name>.vNN.tsrv`, `NN` = 1-based index, data volumes first) is:

```
header   64 B   "TSV\x1A" | version u16 | volume index u16 | N u16 | M u16 | piece size u32 |
                archive size u64 | set id 32 B | payload length u64
payload         the volume's pieces, piece_size bytes each, in stripe order
descriptor      CBOR: set id, archive name, archive size, BLAKE3 of the whole archive, piece size,
                N, M, piece count, stripe count, BLAKE3 of every data piece, BLAKE3 of every
                parity piece, Merkle root of the piece hashes, generator
trailer  56 B   descriptor offset u64 | descriptor length u64 | BLAKE3 of the descriptor 32 B |
                volume index u16 | reserved u16 | "VST\x1A"
```

The descriptor is **identical in every volume**. Losing any separate file therefore loses
nothing: a single surviving volume names the archive, its size and hash, the geometry and every
piece hash. A volume whose header is damaged is still identified by its trailer, and one whose
trailer is damaged by its header plus a descriptor taken from any sibling; a volume is only
useless when both ends are damaged (its pieces would still be verifiable by hash, but the index
could not be trusted without a name heuristic, which is deliberately not used).

The set id is derived, not random: `BLAKE3-derive("tsaur volume set v1", archive hash ‖ archive
size ‖ piece size ‖ N ‖ M)`. Splitting the same archive twice with the same geometry produces
byte-identical volumes, and `repair` recreates a lost volume byte-for-byte.

## 4. Space and time overhead

Measured on the 956,731-byte benchmark archive (`benchmarks/corpus`, default options), every
supported piece size, `split` then `join` verified bit-exact each time:

| Geometry | Piece size | Pieces | Volumes total | Overhead | of which padding |
|---|---:|---:|---:|---:|---:|
| 4+2 | 64 KiB | 15 | 1,514,354 B | 158.3 % | 8.3 % |
| 4+2 | 128 KiB | 8 | 1,577,646 B | 164.9 % | 14.9 % |
| 4+2 | 256 KiB | 4 | 1,576,422 B | 164.8 % | 14.8 % |
| 4+2 | 512 KiB | 2 | 2,100,302 B | 219.5 % | 69.5 % |
| 4+2 | 1 MiB (former fixed default) | 1 | 3,148,674 B | 329.1 % | 179.1 % |
| 4+2 | 4 MiB | 1 | 12,585,858 B | 1315.5 % | 1165.5 % |
| 4+2 | 64 MiB | 1 | 201,329,538 B | 21043.5 % | 20893.5 % |
| 2+1 | 64 KiB | 15 | 1,510,841 B | 157.9 % | 7.9 % |
| 8+2 | 64 KiB | 15 | 1,255,534 B | 131.2 % | 6.2 % |
| 8+2 | 256 KiB | 4 | 1,578,794 B | 165.0 % | 40.0 % |

The parity itself costs `M / N` (150 % for 4+2, 125 % for 8+2); everything above that is
padding: a piece size larger than the archive turns every volume into one mostly empty piece.
A fixed 1 MiB default was therefore wrong for small archives.

**Adaptive default (implemented):** when no piece size is given, `adaptive_piece_size` picks the
smallest power of two between 64 KiB and 1 MiB such that every data volume holds at least 16
pieces (`piece = next_pow2(ceil(size / (16 · N)))`, clamped), and grows beyond 1 MiB (up to the
64 MiB format maximum) only when the archive would otherwise need more than one million pieces
(descriptor size, memory). Worst-case padding is therefore `(1 + M) · piece / size ≤ (1 + M) / (16 N)`
(4.7 % for 4+2) once the archive is above 4 MiB · N, and the 64 KiB floor below that. On the
benchmark archive the rule selects 64 KiB for 4+2, 2+1 and 8+2: 158.3 %, 157.9 % and 131.2 %
respectively, within 8 points of the parity minimum. The chosen size is written into every
volume, so readers never depend on the rule (existing volumes keep their recorded size);
`--piece-size-kib` still forces any supported size. Boundaries are unit-tested
(`adaptive_piece_size_boundaries`): the floor for tiny archives, the switch at exactly 16 pieces
per volume, the 1 MiB ceiling for ordinary sizes, growth past a million pieces, the format maximum.

* Parity: `M / N` of the archive size (4+2 → +50 %, 5+1 → +20 %, 2+1 → +50 %, 8+2 → +25 %).
* Padding: at most `piece_size - 1` bytes on the last piece, plus per-volume header (64 B),
  trailer (56 B) and descriptor (about 80 B + 32 B per data piece + 32 B per parity piece):
  a 1 GiB archive with 1 MiB pieces and 4+2 carries a 41 KB descriptor per volume.
* Memory: split, verify, join and repair hold one stripe (`(N + M) × piece_size`) plus buffers;
  a 4+2 set with 1 MiB pieces needs about 8 MB whatever the archive size.
* Time: split reads the archive twice (once to hash it, once to stripe it) and writes
  `(1 + M/N)` times its size; join reads `N` volumes' worth and writes the archive once. The
  Reed-Solomon coder runs at hundreds of MB/s on one core.

## 5. Placement across devices

`split --out DIR` may be given several times; volumes are assigned round-robin, data volumes
first. Protection is only real when the volumes that can fail together are fewer than `M`:
six volumes on one drive protect against nothing when that drive dies. The command therefore
reports, for the given locations, the largest number of volumes that share one location and
warns when losing that location would exceed the parity. The right default is one location per
volume.

## 6. Failure cases and behaviour

| Situation | Behaviour |
|---|---|
| Up to `M` volumes missing or damaged | `join` rebuilds every stripe; `repair` recreates the missing volume files byte-identical |
| More than `M` pieces of one stripe unavailable | `join`/`repair` stop before writing anything and name the stripe and the volumes that would be needed (exit code 5, missing data) |
| Damaged piece inside a present volume | detected by its BLAKE3 hash and treated as an erasure of that stripe |
| Damaged header or trailer | the other end and the sibling descriptors identify the volume |
| Wrong or unrelated volume among the inputs | grouped by set id; reported as an extra file, never mixed in |
| Two copies of the same volume | the first copy that verifies is used; the other is reported |
| Joined file differs from the original | impossible to pass silently: the joined file is hashed and compared with the archive hash in the descriptor before it gets its final name |
| Encrypted archive | volumes carry ciphertext; the descriptor exposes the archive file name, size, hash and piece hashes, nothing about the plaintext beyond what the archive file itself exposes |

## 7. Why not reuse the `.pieces`/`.par` layout as is

The existing sidecars protect one archive *file* against local damage with stripes of up to 200
consecutive pieces. Volume sets must survive the loss of a *whole device*, which requires that
every stripe spreads exactly one piece per volume (volume-major placement) and that the metadata
travels with every volume. Both mechanisms share the piece hashing, the Merkle root and the
Reed-Solomon coder; only the placement and the container differ.

## 8. Direct exchange (prototype, implemented)

`tsaur volumes serve <volumes> --listen host:port` serves the pieces of the verified volumes it was
started with (read-only, one request per TCP connection, requests of at most 256 bytes:
`SETS`, `DESCRIPTOR <set id>`, `HAVE <set id>`, `PIECE <set id> <volume> <stripe>`).
`tsaur volumes fetch --set <id> [--descriptor <hash>] --from host:port ... --out DIR [--join out.tsr]`
obtains the descriptor (from local volumes when present, else from a peer), verifies it as
described in `VOLUME-TRUST.md`, asks every peer which volumes it holds, chooses the fewest
volumes that make the set reconstructible locally (data volumes first, parity when a data volume
is unavailable everywhere; `--volumes all` or an explicit list otherwise), fetches piece by piece
with hash verification and peer failover, keeps a `.partial` file plus a `.partial.map` so that an
interrupted fetch resumes exactly where it stopped, renames a volume to `.tsrv` only when
complete, and can join with the unchanged offline code. Peer addresses are supplied by hand;
discovery and NAT traversal are outside this package. Tested with two instances on the loopback
interface: needed-only fetch, resume after an interruption, a lying peer whose descriptor and
pieces are rejected while an honest peer is used, set-id-only verification, a peer that lacks
some data volumes (parity fallback), garbage requests, and the CLI end to end.

### 8.1 Resume is decided on disk, not by the map

`<name>.partial.map` records which pieces are believed received; it is a hint. On every resume
the partial file's header must name the same set, index and piece size, and **every piece the
map declares complete is read back and hash-checked** before it is trusted; a damaged or
missing map makes the fetch check every position of the partial file; a short file is extended
and the missing tail treated as absent; a partial file with a foreign header is discarded. The
map itself is written through a temporary file and a rename, so an interruption while it is
being updated leaves the previous map. Tests (`tests/transfer_resume.rs`): a byte flipped in a
piece the map claims, a map cut in half, a deleted map, a map claiming pieces never written, a
file truncated in the middle of a piece, a partial file of another volume index. In every case
the finished volume is byte-identical and the archive joins.

### 8.2 Limits

| What | Limit | Enforced by |
|---|---|---|
| request line | 256 bytes, ASCII, newline-terminated | server and client |
| descriptor | `MAX_DESCRIPTOR` = 64 MiB, refused before it is read; `MAX_PIECES` = 2,000,000 pieces; archive name ≤ 255 bytes without separators, colons or control characters | client, `validate()` |
| `HAVE` / `SETS` replies | 4 KiB / 1 MiB | client |
| piece reply | exactly the piece size, refused before allocation otherwise | client |
| memory | one piece buffer per fetch; the server streams pieces in 1 MiB steps and holds one descriptor per set | by construction |
| temporary space | the full size of every wanted volume is reserved before its first piece (`bytes_reserved` in the report); running out of space fails the fetch with an I/O error and resume continues later | client |
| connections | 64 in total and 8 per source address by default (`--max-connections`, `--max-connections-per-peer`); excess connections get `ERR 503` at once | server |
| request rate | 200 new connections per second per source address by default, twice that in a burst (`--max-requests-per-second`; token bucket); excess gets `ERR 429`; one request is one connection, so this is also the piece rate one address can obtain (a sequential fetch with 1 MiB pieces makes about 120 per second on a gigabit link); the client backs off and retries for about 2.5 s | server, client |
| waiting | 30 s connect, read and write timeouts on both sides | both |
| connection budget | the request line, and a TLS handshake before it, must arrive within 30 s in total; a response of L bytes must be consumed within max(30 s, L / `--min-rate-kib`, default 64 KiB/s); when the budget ends the connection is closed and its slot released, whatever the peer is doing | server |
| local paths | fetched volumes are written only under `--out`, named from the descriptor's archive name when that is a safe single path component and from the set id otherwise; nothing from the wire selects a path | client |
| listening | loopback by default; any other address requires `--expose-lan` together with who may fetch: `--tls-identity FILE --allow <fingerprint>...` or `--allow-anyone`; the resulting access is printed (`access:`) and anonymous access is warned about | CLI |
| TLS | the handshake, including the peer's fingerprint check, completes before any request byte; connections above the limits are closed without a reply in TLS mode (`ERR 503` / `ERR 429` in plain mode) | server |

What is **not** limited: total bandwidth, and the number of distinct source addresses (each
keeps a small token bucket; buckets that are full again are forgotten once more than 4096 are
tracked). Tests (`tests/transfer_limits.rs`): a connection that sends nothing and one that
trickles a byte every 200 ms both lose their slot when the budget ends and the slots are usable
again; a burst of 30 requests from one address at a rate of 5 per second admits about ten and
answers `ERR 429` to the rest, and a fetch through the same server still completes thanks to
the client's back-off; in TLS mode the same refusal is a silent close, retried the same way.

### 8.3 Encrypted transport with locally pinned identities

The same protocol can run inside TLS 1.3 (`rustls`, ring provider; `crates/tsaur-core/src/tls.rs`)
between two self-signed certificates that each side pins by SHA-256 fingerprint. `tsaur volumes
keygen --out peer.key` creates an identity (P-256 key + certificate) and prints its fingerprint;
`serve --tls-identity server.key --allow <fingerprint>...` (or `--allow-anyone`) serves over
TLS; `fetch --from A:port --peer-id <A's fingerprint> --tls-identity client.key` pins the server
and presents the client identity. No certificate authority, account or service is involved;
fingerprints travel through the same trusted channel as the set id. The server completes the
handshake (with the client's fingerprint check) before reading any request; a mismatch on either
side ends the connection before a byte of the protocol is exchanged, and plain and TLS endpoints
refuse each other immediately instead of holding a connection until the timeout. Resume, limits
and the offline join are unchanged. Tests (`tests/transfer_tls.rs`, CLI `keygen_serve_and_fetch_
over_pinned_tls`): identity files round trip; mutual TLS fetch and join; a server with another
certificate is refused with nothing written; clients outside the allow list and anonymous clients
against a mutual server are refused; the anonymous-client mode still pins the server; plain vs TLS
in both directions fails cleanly; an interrupted TLS fetch resumes over TLS with the pieces on
disk re-verified. Trust rules: `VOLUME-TRUST.md` §6.
