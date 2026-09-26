# Changelog

All notable changes to T-saur are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow semantic versioning once
0.1.0 is tagged. Percentages quoted below are archive size relative to the input: lower is better.

## [Unreleased]

## [1.0.0-rc.2] — 2026-09-26 (release candidate 2)

The second candidate: everything below was merged after the rc.1 snapshot of 2026-09-24 and is
covered by the same producer checks (CI on three systems with the determinism comparison on
every change; the Linux gate and campaign of rc.1 apply to the unchanged archive code, the
`--effort` and transfer changes have their own tests). No change to the archive format or the
wire protocol. Note: the crates published on crates.io as 1.0.0-rc.1 were packaged from commit
`13c24b5`, which already contained the `--effort`, network-limit and CI changes listed here;
rc.2 is the first version with a git tag and binary packages.

- **On crates.io**: `tsaur-core` and `tsaur` 1.0.0-rc.1, published by the maintainer on
  2026-09-26 from commit `13c24b5` of this repository (`cargo install tsaur`;
  `.crate` SHA-256 `a1dee8f4dc29fcb4cb10bab8e3459aab73ad8018358faa776cd89dbc5e9588a5` for `tsaur-core`,
  `9b1880280b9796793d81e8b610fa27a4296035149902b864080797e09a0e23b8` for `tsaur`); docs.rs builds the library documentation.

- **README for readers who arrive from a search** (no change to any claim): a plain lead paragraph
  that says what T-saur is and for whom, the original paragraph kept below it; "When to use it,
  and when not", "How it differs from ZIP, 7z, RAR and tar + zstd" and "Questions and answers",
  all drawn from the contract, the guide and the measurements; the MCP server as a section of its
  own; a contents line; "peer-to-peer distribution" corrected to the direct exchange v1 actually
  offers; the stale "once the repository is public" removed. `llms.txt` at the repository root
  points agents at the documents, caveats first.
- **Network limits for the volume exchange** (roadmap 1.1; design in
  `docs/design/plans/1.1-network-limits-and-supply-chain.md` part A, trust rules in
  `docs/design/VOLUME-TRUST.md` §6.3-§6.4): `serve --revoke FILE` and `fetch --revoke FILE`
  refuse certificate fingerprints even when pinned or allowed, checked at start-up (a server
  whose every allowed identity is revoked refuses to start; a fetch whose pinned peer is
  revoked fails before any connection) and again by the TLS verifiers at the handshake;
  `serve --allow-set <set id>=<fp>` and `--allow-file` admit a client to specific sets only,
  with the other sets answered like unknown sets (`404`) so that nothing about them is
  disclosed; `serve --max-bandwidth-kib` caps the bytes sent per second across all connections
  (one shared token bucket; the time a connection waits is credited to its budget, so the cap
  never triggers the slow-client cut-off); `serve --max-peers` caps the distinct source
  addresses served at once (default 64, a resource key rather than an identity). Library:
  `tls::Revocations`, `server_config_with`, `client_config_with`, `transfer::Acl`,
  `Server::bind_acl`, `ServerLimits::{max_bandwidth, max_peers}`, `ClientTls::revoked`
  (`ClientTls` now implements `Default`), `list_peer_with`. The start-up lines report the
  effective lists (`allowed`, `restricted`, `revoked`), the limits (`--json` gains a `limits`
  object) and `readers:` per set. No change to the archive format or the wire protocol: a
  1.0.0-rc.1 client works unchanged against a server with any of these options.
- **Supply-chain checks in CI** (roadmap 1.1): `.github/workflows/supply-chain.yml` runs
  `cargo deny` (RustSec advisories, the license policy of `THIRD-PARTY-NOTICES.md` with the one
  LGPL exception, banned TLS stacks, crates.io as the only source) against `tsaur/deny.toml` on
  every change to the dependency graph, on every push to `main` that changes it, and weekly;
  the weekly and manual runs add a `cargo audit` report. A step fails the day the copyleft
  component stops reaching the lite build, so that the distribution documents are updated in
  the same pull request. Nothing is compiled; the job costs about a minute.
- **Documentation-only pull requests are mergeable under the ruleset**:
  `.github/workflows/ci-docs.yml` reports the required checks with jobs that build nothing, on
  exactly the paths `ci.yml` ignores.

- **`pack --effort 1..5`** (roadmap 1.1, "speed of `--codec best`"; design in
  `docs/design/plans/1.1-codec-sampling.md`): `--codec best` can decide the codec of each block on
  a sample instead of running every codec on every block. The sample is eight evenly spaced 8 KiB
  slices (one more per 128 KiB of block, up to 32; offsets 4-byte aligned so the ARM64 branch
  filter's word alignment survives), never the block prefix (a prefix picks the wrong codec on a
  third of the benchmark blocks). Effort 1 = zstd only, 2 = zstd or xz on the sample (never PPMd:
  fast extraction), 3 = zstd, xz or PPMd on the sample, 4 = 3 plus xz on the whole block whenever
  PPMd wins the sample, 5 = the full trial. Blocks up to 128 KiB always run the full trial; the
  incompressible-block shortcut, the filter heuristics and the tie-break order are the ones of the
  full trial, applied to the sample. `PackOptions::effort` (default 5), `PackReport::blocks_sampled`
  (also in `--json`). **The default output is unchanged byte for byte**: `--effort 5` is the
  unchanged code path, the golden archives are reproduced at every effort (their blobs are below
  the threshold; pinned by a test), and the effort is not recorded in the archive, so no format
  or writer-generation change (`tsaur-core/1.0`). Measured on a shared 4-vCPU Linux container,
  three runs, medians of the `pack` process CPU (`benchmarks/corpus` / `corpus_versions` /
  `corpus_binary` from `build_extra_corpora.py`, here libc, libstdc++ and the `tsaur` binary,
  15.6 MB): effort 3 costs 1.25× / 1.05× / 0.84× the CPU of `--codec zstd` where effort 5 costs
  2.2× / 2.0× / 2.2×, and it loses +0.00 / +0.37 / −0.05 point of ratio against effort 5 (the
  sample judges "looks like text" and the x86 filter on the strided sample rather than on the
  block prefix, which is why machine code gains a little); effort 4 recovers the 0.37 point at
  1.5× zstd; with `--solid 4` every effort 2..4 matches effort 5 within 0.01 point at 1.3–1.5×
  zstd. The default stays 5 in this cycle; a later release may move it to 3 as a
  writer-generation bump.

## [1.0.0-rc.1] — 2026-09-24 (release candidate)

Publication preparation after the tag (documents and tooling only): `CODE_OF_CONDUCT.md`,
`AUTHORS.md` (AI assistance stated), `CITATION.cff`, `docs/INSTALL.md`, `docs/PROVENANCE.md`,
`benchmarks/CORPUS-LICENSES.md`, issue and pull-request templates, a draft-release workflow for
the lite packages, neutral copyright lines in the license files, relative paths in tool output;
on 2026-09-25 the research notes and the name-availability script, the maintainer's working
material, were removed from the public tree (`docs/PROVENANCE.md`); the same day the maintainer
settled the copyright holder (Iulian Bondari, license files updated) and published a contact
address (`AUTHORS.md`). A dependency audit the same day found that the LGPL-3.0 crate `cabac` is a
dependency of `preflate-rs`, not only of Lepton, so it is part of **every** build; the statements
that the lite build contains no LGPL code were corrected everywhere, and the lite packages now
carry the LGPL texts too (`THIRD-PARTY-NOTICES.md`, `docs/DISTRIBUTION-POLICY.md`).

- **Format v1 frozen** (`docs/spec/TSAUR-FORMAT-SPEC-v1.0.md` §12, `docs/V1-CONTRACT.md`): v1 readers
  accept exactly version 1 of `.tsr` and `.tsrv` and refuse any other version by name; the writer
  records `tsaur-core/1.0` as generator and reproduces the golden archives byte for byte
  (`tsaur/crates/tsaur-core/tests/golden/`, created by the release binaries of both builds; read
  by both in `tests/golden.rs` and by `tools/compat_check.py`).
- Untrusted-data review: `unpack` refuses to replace existing files unless `--overwrite` is given
  and checks every destination before writing anything; symbolic links inside inputs are skipped
  when packing (never followed, never recreated); reader ceilings for the number of entries
  (10,000,000) and path depth (256 components); an intact `.tsrv` header of another format version
  disables the volume instead of being treated as damage; the PDF text converter's panics are
  contained (`pack --canonical` keeps going, the entry gets a note).
- `pack --no-lepton` in the full build writes archives that the lite build can read (byte-identical
  to the lite build's own output). MCP `tsaur_unpack` gains `overwrite`.
- Robustness campaign on the stable toolchain (`tests/robustness.rs`): damaged archives, volumes,
  descriptors, transfer requests and replies, resume files; findings are saved as regression inputs.
- Distribution: the lite build is the recommended public binary, the full build an explicit second
  package with its LGPL notices (`docs/DISTRIBUTION-POLICY.md` §3); `tools/package.py` builds and
  checks both packages from a clean directory; `docs/GUIDE.md` is executed by `tools/check_guide.py`.
- `keygen --out FILE` also writes `FILE.pub` (the Ed25519 public key) so that `verify --pubkey` can read it.
- Version 1.0.0-rc.1; `tsaur-core` dependency of the CLI carries a version for packaging.

## [0.1.0] — development history up to the release candidate

### Format (`.tsr`, container version 1)
- Header `TSR\x1A`, section table (CBOR, CRC-32), trailer `RST\x1A`; sections: recipients,
  dictionary, blobs, chunk index, manifest, signatures; every section BLAKE3-hashed.
- Content-defined chunking (FastCDC, normalised level 2; profiles fine / p2p / archive), BLAKE3
  chunk identities, Merkle root as the archive identity, deduplication.
- Blob records `{off, clen, ulen, codec, n, p, f, d}`: codecs store / zstd / zstd + dictionary /
  xz (LZMA2) / PPMd H / zstd + chunk dictionary (delta); per-blob parameters `p`; pre-filter byte
  `f` (x86 BCJ, ARM64 BCJ); dictionary chunk list `d` for delta blobs.
- Container recipes: ZIP/OPC members and PDF FlateDecode streams inverted with preflate
  (corrections stored, rebuilt bit-exact), JPEG files and DCTDecode streams recoded with Lepton.
- External chunk references (`x = 1`) and the `refs` manifest list (compression by reference).
- Hybrid fidelity: derived entries `.tsaur/views/<path>.md|.txt` with `derived {from, view, generator,
  tokens_est}` (stored canonical views of DOCX/PDF entries).
- Encryption: XChaCha20-Poly1305 per blob and section, random archive key, stanzas `argon2id`
  and `x25519mlkem768`; Ed25519 signature over the header and section table.
- Safety limits recorded in the manifest and enforced by readers; sanitised entry paths.
- Sidecars `.pieces` (piece hashes, Merkle root, stripes) and `.par` (Reed-Solomon parity).

### Implementation (`tsaur-core`, `tsaur` CLI)
- Streaming packer with parallel block compression bounded by threads and 128 MiB in flight;
  memory-mapped reader; streaming verify and extract (temporary name, rename after hash check).
- Block mode (64 KiB chunks, 1 MiB blocks) as default; `--solid N`; `--granular` with a trained
  zstd dictionary; `--codec best` tries zstd, xz and (on text) PPMd per block and keeps the smallest;
  x86 / ARM64 branch filters are tried on blocks that look like machine code; blocks that zstd
  cannot shrink by 3 % skip the slower codecs.
- Delta coding (on by default, `--no-delta` to disable): entries recognised as new versions (same
  path in a `--ref` archive, or a similarly named earlier entry) are compressed with zstd against
  the earlier content as dictionary when that is smaller; blocks never mix dictionaries; readers
  refuse dictionary chains and oversize dictionaries.
- Reference archives (`--ref`), hybrid post-quantum recipients (`keygen --recipient`, `pack --to`,
  `--identity`), Argon2id passwords (`--kdf-memory-mib`), Ed25519 signing (`--sign-key`).
- Agent operations: `list --md` with token estimates, `stat`, `grep`, `read --bytes/--lines`,
  `read --view canonical` (DOCX → Markdown, PDF → text; served from the stored view when present),
  `pack --canonical` (store the views at pack time, hybrid fidelity; default extraction skips them),
  `unpack --entry <glob>`, `verify --json`, `info` (structure and statistics; works on locked
  archives), stable exit codes.
- `tsaur mcp`: Model Context Protocol server over stdio, dual-era (revision 2026-07-28 per-request
  versioning with `server/discover` and `UnsupportedProtocolVersionError`, plus the legacy `initialize`
  handshake for 2025-11-25 and earlier clients); tools `tsaur_info`, `tsaur_list`, `tsaur_stat`,
  `tsaur_read`, `tsaur_grep`, `tsaur_diff`, `tsaur_verify`, `tsaur_unpack`; resources
  `tsaur://<archive>/<entry>`; `--root` sandbox; archive content labelled as untrusted data.
- `diff` between two archives (entries added / removed / changed / unchanged, chunks shared with the
  older archive = what `--ref` would save) and citation URIs `tsaur://<merkle-root>/<path>[#L a-b | #B a-b]`
  in `stat`, `list --json` and MCP reads.
- Pieces and parity: `pieces`, `recover`, `verify --pieces`; everything streams one stripe at a time.
- Offline volume sets (`tsaur volumes split | inspect | join | repair`): an archive striped across N data
  + M parity volumes (`.tsrv`) over directories or drives; any N of N + M rebuild it bit-exact; every
  volume carries the full descriptor (no index file); damaged headers/trailers/pieces tolerated within
  parity; repair recreates lost volumes byte-identical; placement warning when one location holds more
  volumes than the parity. Design note: `docs/design/VOLUME-SETS.md`.
- Adaptive default piece size for volume sets (64 KiB .. 1 MiB, about 16 pieces per data volume;
  the 957 KB benchmark archive costs 158 % in 4+2 instead of 329 % with a fixed 1 MiB piece);
  `descriptor_b3` reported by `split` and `inspect`.
- Direct piece exchange prototype: `tsaur volumes serve` (read-only, manual addresses) and
  `tsaur volumes fetch` (verified descriptor and pieces, fewest volumes needed, peer failover,
  resume from `.partial` files, optional join). Trust contract: `docs/design/VOLUME-TRUST.md`.
- Transfer stabilisation: resume re-verifies every piece from disk (the progress map is a hint,
  written atomically); explicit limits for descriptor size, piece count, reply sizes, memory,
  reserved space, connections (total and per peer) and timeouts; pieces streamed by the server;
  fetched volumes named from the set id when the descriptor's archive name is not a safe path
  component; `serve` refuses non-loopback addresses without `--expose-lan`.
- Encrypted transport with locally pinned identities: `tsaur volumes keygen` / `fingerprint`
  (self-signed P-256 certificate, SHA-256 fingerprint), `serve --tls-identity --allow ... |
  --allow-anyone`, `fetch --peer-id ... [--tls-identity]`; TLS 1.3 via `rustls` (ring), no
  certificate authority, account or service; handshake and fingerprint check before any request;
  plain and TLS endpoints refuse each other at once. Serving continues when stdout is closed.
- Explicit network access: `--expose-lan` must be paired with `--allow <fingerprint>` (TLS) or
  `--allow-anyone`; the server prints the resulting access (encryption, server identity, client
  authorization) and warns when clients are anonymous. Per-address request rate
  (`--max-requests-per-second`, 200/s, burst 400, `ERR 429`) and a time budget per connection
  (request within 30 s, responses consumed at `--min-rate-kib` or faster) release the slots of
  idle, trickling or non-reading peers; clients back off on `429`/`503` or a silent close.
  `fetch --stop-after N` for reproducible interruptions; `fetch` reports connect, TLS handshake,
  transfer and join times separately. Two-device benchmark tooling (`benchmarks/two_device_bench.py`)
  and an independent-review package description (`docs/review/REVIEW-PACKAGE.md`).
- `list`, `info` and `verify` report `requires` (reader features an archive needs, today `lepton`)
  and `missing_features` for the running build; distribution policy and build/format matrix in
  `docs/DISTRIBUTION-POLICY.md`; `THIRD-PARTY-NOTICES.md` and `licenses/` (LGPL-3.0, GPL-3.0 texts).
- Extraction refuses entry sets that differ only by letter case on case-insensitive filesystems
  (Windows, macOS) instead of overwriting silently; explicit selection still works. Paths longer
  than the classic Windows limit round-trip.
- MCP: every registered archive also exposes a generated `tsaur://<archive>/TSAUR.md` overview
  resource (the Markdown listing with token estimates).
- Crate metadata for crates.io (keywords, categories, readme, `rust-version = 1.87`).
- Tests: round trips, determinism (including `--jobs 1` vs `--jobs 8`), random corruption and
  truncation never panic, corrupt `.pieces`/`.par` sidecars never panic and never alter the archive,
  unsafe paths rejected, case collisions, long paths, reference archives, JPEG/PDF/DOCX bit-exact
  rebuilds, hybrid recipients, hybrid views, multi-stripe recovery, filter round trips, CLI exit
  codes, MCP end-to-end session (both protocol eras); clippy `-D warnings` and rustfmt clean.

### Dependency licenses
- All 224 dependency packages are permissive (MIT / Apache-2.0 / BSD / Zlib / 0BSD / CC0 / Unicode) except
  `cabac 0.15.0` (LGPL-3.0-or-later, pulled in by `lepton_jpeg`). Lepton support is now a Cargo feature
  (`lepton`, on by default) so that builds without LGPL code are possible; see README "Licenses".
  *Corrected 2026-09-25:* `cabac` also reaches every build through `preflate-rs`, so no build is
  free of it; see the 1.0.0-rc.1 entry above.

### Measured (see `benchmarks/RESULTS-rust.md`)
- Corpus A (10 unrelated documents): 62.2 % vs 7-Zip 73.6 % / WinRAR 74.2 %.
- Corpus B (with edited versions): 45.2 % with delta coding in block mode (48.1 % without; solid 44.3 %) vs 55.0 % / 55.6 %.
- Real-world DOCX/PDF/JPEG set (13 files): 78.3 % vs 92.1 % / 92.3 %, 13/13 containers inverted.
- Windows x64 executables (13.5 MB): 32.5 % default, 30.8 % solid vs 7-Zip BCJ2 29.7 %, WinRAR 33.1 %.
- ARM64 shared libraries (22.5 MB): 17.8 % default, 16.0 % solid vs 7-Zip 16.4 % (15.9 % with filters off), WinRAR 18.5 %.
- Romanian text with diacritics (437 KB, a private corpus that is not published, so this figure cannot be reproduced from the repository): 26.6 % vs 7-Zip LZMA2 31.0 % / PPMd 28.6 % / WinRAR 32.6 %.
- Incremental archive by reference: 2.8 % of the input (6.8 % without delta coding) instead of 45.2 % for a full archive.
