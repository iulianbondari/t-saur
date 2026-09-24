# tsaur — Rust reference implementation of the T-saur (`.tsr`) format

Workspace with two crates:

| crate | what it is |
|---|---|
| `tsaur-core` | library: on-disk framing (`format`), FastCDC chunking + BLAKE3 (`chunk`), codecs and branch filters (`codec`), ZIP/OPC, PDF and JPEG container recipes (`container`), envelope encryption + hybrid recipients + signatures (`crypto`), manifest model (`manifest`), streaming packer (`pack`), memory-mapped reader / verifier / extractor and credential-free `inspect` (`read`), streaming pieces + Reed-Solomon parity sidecars (`pieces`), offline volume sets (`volumes`), direct piece exchange (`transfer`), pinned-fingerprint TLS identities (`tls`), canonical views (`canonical`), path sanitisation (`paths`) |
| `tsaur` (`tsaur-cli`) | the command-line tool; `ops` = operations shared by the CLI and the MCP server, `mcp` = the Model Context Protocol server (`tsaur mcp`) |

## Build and test

```bash
cargo build --release
cargo test --release
cargo clippy --workspace --all-targets -- -D warnings
```

Tests cover: granular round trip with dictionary and dedup, solid + encrypted + signed round trip (wrong password
rejected, foreign key rejected), byte-identical determinism, corruption detection (150 random flips + 15 truncations
never panic), range reads, container (ZIP, PDF, JPEG) explode/rebuild, reference archives, hybrid post-quantum
recipients, multi-stripe pieces recovery (damage, truncation, appended garbage, beyond-parity), branch-filter round
trips on adversarial inputs, unsafe path rejection, CLI exit codes, `info` on locked archives, `diff`, and an
end-to-end MCP session (both protocol eras).

## CLI

```text
tsaur pack   <out.tsr> <inputs...> [--block <MiB> | --block-kib <KiB> | --solid <MiB> | --granular]
             [--chunk fine|p2p|archive] [--level N] [--codec best|zstd|xz|ppmd] [--jobs N] [--no-dict] [--no-container] [--no-delta]
             [--ref base.tsr ...] [--password PW | TSAUR_PASSWORD env] [--kdf-memory-mib N] [--to recipient.pub ...]
             [--sign-key FILE] [--keep-mtime] [--timestamp] [--pieces] [--piece-size-kib N] [--parity-pct N]
             [--canonical] [--json]

Modes (all bit-exact, all verified per chunk):
  default   64 KiB chunks for dedup/verification, 1 MiB compression blocks  -> random access per block, ratio close to solid
  --solid N large blocks (e.g. 8 MiB)                                        -> best ratio
  --granular one chunk per blob + trained dictionary                         -> best random access / P2P fetch, weakest ratio
Codecs per block: --codec best (default: zstd -19, xz/LZMA2 9e and — on text-like blocks — PPMd H are tried, the
smallest is kept; blocks zstd cannot shrink by 3 % are stored; x86 and ARM64 branch filters are tried on machine
code), zstd (fast: --level 9 ≈ 200 MB/s), xz, ppmd. Delta coding (--no-delta to disable): an entry that is a new
version of content the archive or a --ref archive already holds is also tried with zstd against that content as
dictionary (codec "zstd+delta", blob field `d` = dictionary chunks); the smaller encoding wins.
Containers: ZIP/OPC members and PDF FlateDecode streams are inverted with preflate; JPEG files and DCTDecode streams
are recoded losslessly with Lepton (~22 % smaller). Everything is rebuilt bit-exact or stored raw.
Reference archives: `pack --ref base.tsr` stores only the chunks that base.tsr does not already hold ("compression by
reference"; the manifest records the reference root and counts); readers resolve them with the same `--ref`.
Hybrid fidelity: `pack --canonical` also stores the Markdown/text view of every DOCX/PDF as `.tsaur/views/<path>.md|.txt`
(manifest `derived {from, view, generator, tokens_est}`); `read --view canonical` serves it, `unpack` skips views
unless they are selected, a view that cannot be generated leaves a note on the source entry.

tsaur unpack <archive> <dir> [--entry PATH|GLOB ...] [--json]
tsaur list   <archive> [--md] [--json]            # --md = TSAUR.md agent view with token estimates; --json adds citation URIs
tsaur info   <archive> [--json]                   # sections, codecs, filters, chunks, blobs, refs; works on locked archives
tsaur stat   <archive> <entry> [--json]           # hash, mode, chunks, container details, token estimates, citation URI
tsaur grep   <archive> <regex> [--entries GLOB] [-i] [--max N] [--json]   # search without extracting
tsaur read   <archive> <entry> [--bytes A-B | --lines A-B] [--view raw|canonical]   # canonical = DOCX->Markdown, PDF->text
tsaur diff   <a.tsr> <b.tsr> [--json]             # entries added/removed/changed; chunks of B already in A (= what --ref saves)
tsaur verify <archive> [--pubkey HEX] [--pieces] [--json]
tsaur pieces <archive> [--piece-size-kib N] [--parity-pct N]
tsaur recover <archive>
tsaur volumes split   <archive> --data N --parity M [--piece-size-kib K] --out DIR ...   # N + M .tsrv volumes, round-robin over the DIRs; piece size adaptive by default
tsaur volumes inspect <files|dirs ...> [--verify]   # sets found, missing volumes, reconstructible?; exit 5 when not
tsaur volumes join    <out.tsr> <files|dirs ...>    # any N of N + M volumes rebuild the archive (hash-verified before rename)
tsaur volumes repair  <files|dirs ...> [--out DIR]  # recreate missing/damaged volumes byte-identical
tsaur volumes serve   <files|dirs ...> [--listen 127.0.0.1:7407] [--expose-lan (--tls-identity FILE --allow FP ... | --allow-anyone)] [--max-connections N] [--max-connections-per-peer N] [--max-requests-per-second N] [--min-rate-kib K]   # serve pieces (read-only)
tsaur volumes fetch   --set HEX [--descriptor HEX] --from HOST:PORT ... [--peer-id FP ...] [--tls-identity FILE] --out DIR [--local PATH ...] [--volumes needed|all|1,2] [--join OUT.tsr] [--stop-after N]
tsaur pack ... --no-lepton               # full build: store JPEGs as they are, so the lite build can read the archive
tsaur unpack ... --overwrite             # replace existing files (refused otherwise, before anything is written)
tsaur volumes keygen  --out FILE        # peer identity for TLS transfers: FILE (private key) + FILE.crt; prints the fingerprint
tsaur volumes fingerprint FILE          # fingerprint of an existing identity
tsaur keygen --out FILE                 # Ed25519 signing key: FILE (hex seed, secret) + FILE.pub (public key for verify --pubkey)
tsaur keygen --recipient --out FILE     # hybrid X25519 + ML-KEM-768 identity: FILE (secret) + FILE.pub (recipient key)
tsaur pack   ... --to alice.key.pub --to bob.key.pub [--password PW]   # one stanza per recipient/passphrase
tsaur unpack ... --identity alice.key   # any one recipient (or the passphrase) unlocks the archive key
tsaur mcp    [--root DIR ...] [--archive FILE ...] [--ref ...] [--identity ...]   # MCP server on stdin/stdout
```

Exit codes: 0 ok · 1 I/O · 2 corrupt or hash mismatch · 3 refused by policy · 4 resource limit · 5 missing data ·
6 cryptography · 7 invalid argument.

## MCP server

`tsaur mcp` speaks JSON-RPC over stdio, one message per line. It implements the current Model Context Protocol
revision (2026-07-28: per-request `_meta` protocol version, `server/discover`, `UnsupportedProtocolVersionError`) and
the legacy `initialize` handshake (2025-11-25 and earlier), so any client works. Tools: `tsaur_info`, `tsaur_list`,
`tsaur_stat`, `tsaur_read` (byte/line ranges, canonical views, base64 for binary content, citation URI), `tsaur_grep`,
`tsaur_diff`, `tsaur_verify`, `tsaur_unpack`. Archives registered with `--archive` are listed as resources
`tsaur://<file name>/<entry>`. Only paths below `--root` (default: the current directory) can be opened or extracted.
Tool failures come back as results with `isError: true` and the CLI exit code in `structuredContent.exit_code`.

## Format v1 (implemented subset of the spec)

```
header 32 B  "TSR\x1A" | version u16 | flags u16 | header_len u32 | reserved
sections     recipients (cleartext CBOR stanzas: argon2id / x25519mlkem768) | dictionary | blobs | chunk index | manifest | signatures
table        CBOR array {t, off, len, blake3}
trailer 24 B table_off u64 | table_len u64 | crc32 | "RST\x1A"
```

* chunk identity: BLAKE3-256; Merkle root over the chunk table = content identity (`root`), also the base of
  citation URIs `tsaur://<root>/<path>#L10-20`
* blobs: `{off, clen, ulen, codec, n, p, f, d}` — one chunk (granular, with a trained zstd dictionary) or a block of
  consecutive unique chunks; codecs store / zstd / zstd+dict / xz / PPMd / zstd+delta chosen per block; `p` = codec
  parameters, `f` = pre-filter (low nibble: 0 none, 1 x86 BCJ, 2 ARM64 BCJ; high nibble: filter parameter),
  `d` = dictionary chunks of a delta blob (never delta blobs themselves; external chunks resolve through `--ref`)
* containers: ZIP-based files parsed with a strict framing parser that keeps every header byte; deflate
  members are inverted with **preflate** (plaintext + a few bytes of corrections) and recreated bit-exact
  whatever encoder produced them; stored members are chunked as-is; anything else (encrypted members,
  unknown methods, ZIP64, gaps) falls back to storing the file raw — the rebuild is verified at pack time
* embedded streams: PDF `/FlateDecode` objects are inverted like ZIP members (zlib header and Adler-32 kept);
  `/DCTDecode` objects and standalone JPEG files are recoded with Lepton (single-threaded, deterministic)
* references: chunks flagged external (`x = 1`) live in an archive listed in the manifest `refs`; readers resolve
  them with `--ref` and report what stays unresolved
* encryption: XChaCha20-Poly1305 per blob and per structured section, keys derived from a random archive key
  (BLAKE3 `derive_key`); the archive key is wrapped once per stanza: Argon2id(passphrase) — reader-enforced floor
  64 MiB / t=3 — and/or hybrid **X25519 + ML-KEM-768** recipients (FIPS 203; KEK = HKDF-SHA-256 over both shared
  secrets, so confidentiality survives the break of either component — "harvest now, decrypt later" protection)
* signature: Ed25519 over header + section table (hash, offset, length of every section)
* limits: declared totals, per-entry size and expansion ratio are checked before allocation; paths are labels
  (NFC, relative, no `..`, no drive letters, no `:`/ADS, no reserved device names)
* pieces: `<archive>.pieces` (BLAKE3 per piece + Merkle root, stripe table) and `<archive>.par` (systematic
  Reed-Solomon, GF(2^8), stripes ≤ 200 data pieces and ≤ 64 MiB); creation, verification and `recover` stream one
  stripe at a time
* volume sets: `.tsrv` files = 64 B header (`TSV\x1A`, index, N, M, piece size, archive size, set id, payload length)
  + pieces + CBOR descriptor (archive name/size/BLAKE3, geometry, every piece and parity hash, Merkle root) + 56 B
  trailer; one piece per volume per stripe, any N of N + M volumes rebuild the archive; memory `(N + M) × piece size`

Not yet: section-aware executable filtering, float-split codec, ML-DSA composite signatures, stored embeddings
and summaries, exact token counts, P2P transport (iroh), RaptorQ.
