# T-saur v1.0 — the contract

This page says what version 1.0 of T-saur promises, what it deliberately leaves out, and how a
promise can be checked. Anything not listed here is not part of v1.0, whatever the code happens
to do. Percentages elsewhere in the documentation are archive size relative to the input: lower
is better.

## 1. Promises

| # | Promise | Where it is checked |
|---|---|---|
| P1 | **Bit-exact archiving and restoration.** `pack` then `unpack` returns every input file byte for byte, including ZIP/OPC documents, PDFs and JPEGs that were taken apart for compression. Every restored byte is verified against a BLAKE3 hash before it is written under its final name. | `tests/roundtrip.rs`, `tests/golden.rs`, the benchmark round trips |
| P2 | **Deterministic output.** The same inputs, options and build produce the same archive bytes on every platform (`--timestamp` and encryption are the documented exceptions). | `deterministic_output`, `parallel_packing_is_deterministic`, the CI determinism job, `tests/golden.rs` (writer freeze) |
| P3 | **Safe listing, info, verification and extraction of untrusted archives.** A damaged or hostile archive yields an error, never a panic, never unverified bytes, never a file outside the destination, never a replaced file unless `--overwrite` was given. Declared sizes, expansion ratio, entry count, path depth and chunk references are bounded before allocation. | `SECURITY.md`, `tests/robustness.rs`, `rejects_unsafe_paths_on_extract`, `random_corruption_never_panics_and_is_detected` |
| P4 | **Offline volume sets.** `volumes split` stripes an archive over N data + M parity volumes; any N of the N + M volumes rebuild it bit-exact (`join`), and lost or damaged volumes are recreated byte-identical (`repair`). Every volume is self-describing; there is no index file to lose. | `tests/volumes.rs`, `tests/golden.rs` |
| P5 | **Direct exchange between two instances**, with the caller's addresses, over plain TCP on loopback or over TLS 1.3 with identities pinned by fingerprint: every descriptor and piece is verified against what the receiver already expects before it is written; resume re-verifies from disk; network data never chooses a local path; limits on sizes, counts, connections, request rate and connection time are explicit. | `tests/transfer*.rs`, `docs/design/VOLUME-TRUST.md`, `docs/design/VOLUME-SETS.md` §8 |
| P6 | **Agent operations.** `list --md`, `stat`, `grep`, `read --bytes/--lines`, `diff`, citation URIs and the `tsaur mcp` server give hash-verified content and label it as data. Canonical views (DOCX → Markdown, PDF → text) are explicitly *semantic*, never bit-exact, and are stored only with `--canonical`. | `tests/cli.rs` (`info_and_mcp_server`), `hybrid_views_are_stored_and_served` |
| P7 | **Encryption at rest and in transit are separate and both optional.** `pack --password` / `--to recipient.pub` encrypts the archive (XChaCha20-Poly1305 per blob, Argon2id and/or hybrid X25519 + ML-KEM-768 recipients, Ed25519 signatures); volumes and transfers carry the archive bytes as they are, so an unencrypted archive stays readable by whoever holds its volumes or its pieces. `serve/fetch --tls-identity` encrypts the transport only. | `solid_encrypted_signed_roundtrip`, `hybrid_pq_recipients_roundtrip`, `tests/transfer_tls.rs` |
| P8 | **Two builds, one format.** The full build (Lepton JPEG recompression, LGPL-3.0 component) and the lite build (`--no-default-features`, no LGPL code) write the same format; `pack --no-lepton` makes the full build write archives every build can read; an archive reports `requires: lepton` when it needs the full build. | `docs/DISTRIBUTION-POLICY.md`, `tests/golden.rs` |
| P9 | **No infrastructure.** Nothing in P1–P8 needs an account, a network service, a key server, a blockchain or a relay. | by construction; see `README.md` |

## 2. Format freeze (v1)

* `.tsr`: header magic `TSR\x1A`, version `1`; section table, chunk index, blob records with
  codec ids 0 (store), 1 (zstd), 2 (zstd + dictionary), 3 (xz), 4 (PPMd H), 5 (zstd + chunk
  dictionary) and filter ids 1 (x86 BCJ), 2 (ARM64 BCJ); manifest keys as listed in the
  specification; recipients stanzas `argon2id` and `x25519mlkem768`; Ed25519 signature block.
* `.tsrv`: header magic `TSV\x1A`, version `1`, the CBOR descriptor and the `VST\x1A` trailer.
* `.pieces` / `.par` sidecars as specified.
* **Readers of v1 accept exactly version 1.** A header that declares any other version is
  refused with "unsupported format version N" (exit code 2), never read with v1 rules. A future
  version will change the number; v1 readers will never misread it as v1.
* **Unknown manifest keys are ignored** by v1 readers and must be preserved by rewriters;
  unknown codec ids, filter ids, stanza types or section types are refused (exit 7 or 2), never
  skipped.
* The golden archives in `tsaur/crates/tsaur-core/tests/golden/` are the frozen reference:
  every v1 reader must open them and restore the committed inputs bit-exact, and the v1 writer
  must reproduce them byte for byte from the same inputs and options. Changing those bytes means
  changing the format, which v1.x may not do.

## 3. Explicitly outside v1.0

Automatic peer discovery, DHT, relays and NAT traversal; blockchain anchoring; model-based,
context-mixing and float-split codecs; section-aware executable filtering; embeddings and
summaries inside archives; richer converters (PPTX/XLSX, OCR); composite post-quantum
signatures; GUI or shell integration; in-place update of archives; Windows symbolic-link or
hard-link restoration (links are skipped when packing and never created when unpacking).

## 4. Limits a v1 reader enforces

| Limit | Value |
|---|---|
| declared total size of an archive's content | `limits.max_total` from the manifest, at most 64 GiB |
| declared size of one entry | 1 GiB (`limits.max_entry`) |
| expansion ratio (declared content / archive bytes) | ≤ 1000 |
| structured section (manifest, chunk index) | 1 GiB |
| entries in a manifest | 10,000,000 |
| path length / depth | 4096 bytes / 256 components |
| blob cache, preflate plaintext, Lepton input, xz decoder memory, PPMd model | 64 MiB, 256 MiB, 64 MiB, 1 GiB, 1 GiB |
| delta dictionary / chain depth | 64 MiB / 16 |
| volume set: piece size, volumes, descriptor, pieces | 64 KiB–64 MiB, ≤ 256, ≤ 64 MiB, ≤ 2,000,000 |
| transfer (per `docs/design/VOLUME-SETS.md` §8.2) | request line 256 B, replies capped, 64 connections / 8 per address, 200 requests per second per address, 30 s and 64 KiB/s budgets |

## 5. Platforms

v1.0 is verified on the platforms where the producer actually ran the full release gate
(`tools/release_gate.py`: both builds, both test suites, the robustness campaign, the golden
archives, the guide, the packages) and recorded the result:

| Platform | What ran | Record |
|---|---|---|
| **Windows 11 x64** (MSVC toolchain, rustc 1.93.1) | release gate, 60 s robustness campaign | `docs/review/RC1-VERIFICATION-REPORT.md` |
| **Linux x64** (glibc 2.39, rustc 1.94.1, cloud container with 4 logical CPUs) | release gate on commit `a8ef56e`, 60 s robustness campaign; a separate 300 s campaign, 8 targets, no findings | `docs/review/RC1-VERIFICATION-REPORT-linux-x64.md`, `docs/review/ROBUSTNESS-CAMPAIGN-linux-x64.md` |

**macOS** (arm64, GitHub `macos-latest`) has run the CI matrix only (`.github/workflows/ci.yml`,
run 36099317045 on 2026-09-25: release build, both test suites including the short robustness
run, clippy, rustfmt, the golden archives read by both binaries, the guide with both binaries,
and the determinism fixture, whose archive hash was identical on Linux, macOS and Windows). That
covers the suite but not the release gate, so macOS is **covered by CI, not declared verified**
until someone runs the gate there and records the result. Windows and Linux ran the same CI
matrix in that run as well.

## 6. What "verified" means here

Every check above was run by the producer of the code. That is verification, not an independent
audit. `docs/review/REVIEW-PACKAGE.md` describes the package prepared for an evaluator who is
not the producer; `SECURITY.md` describes how to report what such a review finds.
