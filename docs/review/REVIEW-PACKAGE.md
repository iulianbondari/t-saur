# Review package: the v1.0 release candidate

This describes the material for an **independent review by another evaluator**. The checks the
producer ran (tests, clippy, rustfmt, the robustness campaign, the measurements in `benchmarks/`,
all listed in `RC1-VERIFICATION-REPORT.md`) are the producer's own verification; they are listed
so the reviewer knows what was and was not looked at, not as a substitute for the review.

## 1. What to review, identified by commit

* Repository: this one, local until v1. The reviewed state is the annotated git tag
  **`v1.0.0-rc.1`** (`git show v1.0.0-rc.1` prints its commit and this file's version); the
  earlier tag `review-1` marks the state before the v1.0 work.
* Source package: `git archive --format=zip -o dist/tsaur-1.0.0-rc.1-src.zip v1.0.0-rc.1`
  produces a zip whose SHA-256 is recorded in `dist/SHA256SUMS` next to it (outside version
  control) and in the delivery message; the golden archives, the reference inputs and the
  verification report are inside the tree. Verify the hash before reading.
* Build: Rust 1.87 or newer, `cd tsaur && cargo build --release`; the lite build is
  `cargo build --release --no-default-features` (no Lepton, no LGPL code). `docs/V1-CONTRACT.md`
  states what the candidate promises; `tools/release_gate.py` reruns every producer check.

## 2. Scope of this round

In scope (all in `tsaur/crates/`):

| Area | Files | What it claims |
|---|---|---|
| offline volume sets | `tsaur-core/src/volumes.rs`, `tsaur-core/tests/volumes.rs` | any N of N+M `.tsrv` volumes rebuild the archive bit-exact; damaged headers, trailers and pieces tolerated within parity; repair byte-identical; adaptive piece size |
| direct exchange | `tsaur-core/src/transfer.rs`, `tsaur-core/tests/transfer*.rs` | descriptor and pieces verified against user-supplied identities before anything is written; resume re-verified from disk; explicit limits (sizes, counts, connections, request rate, time budgets); network data never selects a local path |
| pinned TLS | `tsaur-core/src/tls.rs`, `tsaur-core/tests/transfer_tls.rs` | TLS 1.3 (`rustls`, ring) with self-signed certificates pinned by SHA-256 fingerprint on both sides; no CA, account or service |
| CLI surface | `tsaur-cli/src/main.rs` (`volumes` subcommands), `tsaur-cli/tests/cli.rs` | loopback by default; network exposure needs `--expose-lan` and an explicit `--allow`/`--allow-anyone`; access printed and anonymous access warned about |
| documents | `docs/design/VOLUME-SETS.md`, `docs/design/VOLUME-TRUST.md`, `SECURITY.md` | the design, the limits table and the trust contract the code is supposed to implement |
| archive reader and extraction | `tsaur-core/src/read.rs`, `paths.rs`, `manifest.rs`, `format.rs`; `tests/roundtrip.rs`, `tests/robustness.rs`, `tests/golden.rs` | every byte verified before use; ceilings enforced before allocation; paths never leave the destination; existing files replaced only on request; format v1 accepted exactly |
| containers and views | `tsaur-core/src/container.rs`, `canonical.rs` | bit-exact rebuild of ZIP/OPC, PDF streams and JPEGs; converter failures contained |
| archive encryption | `tsaur-core/src/crypto.rs` | XChaCha20-Poly1305 per blob, Argon2id and hybrid X25519 + ML-KEM-768 stanzas, Ed25519 signatures |

Out of scope for this round: the codecs' compression efficiency (`codec.rs`, `pack.rs` heuristics),
the MCP server's protocol details, the benchmark numbers.

## 3. What the producer checked (not an independent review)

* `cargo test` in the default build and with `--no-default-features`, `cargo clippy --all-targets
  -- -D warnings`, `cargo fmt --check`, on Windows 11 x64 (`RC1-VERIFICATION-REPORT.md`) and on
  Linux x64 (`RC1-VERIFICATION-REPORT-linux-x64.md`); the CI
  matrix (Linux, macOS, Windows) ran the same suites with identical determinism hashes.
* Robustness tests written by the producer: random corruption of volumes never panics and never
  yields wrong bytes; damaged, truncated, foreign or lying progress maps; oversized and malformed
  replies refused before allocation; garbage requests; archive names from the network never
  choosing local paths; wrong server certificate, unlisted client, plain/TLS mismatch; idle and
  trickling connections releasing their slots; per-address request rate with client back-off.
* Measurements on one machine only (`benchmarks/RESULTS-rust.md`); the two-device plan has
  tooling but has not been run on two devices.

## 4. Known limits the reviewer should not need to rediscover

* One connection per request (piece); no pipelining, no parallel connections per fetch.
* Rate limiting is per source address (shared by hosts behind one NAT); total bandwidth and the
  number of distinct addresses are not limited.
* Identities are files without revocation; losing a key means re-pinning on the other side.
* With `--set` alone (no `--descriptor`), piece hashes are provisional until `join` verifies the
  archive hash.
* `--allow-anyone` lets anyone who reaches the port read every served piece, encrypted or not.
* Timing side channels, fingerprinting of traffic patterns and the platform's own TCP stack are
  not addressed.

## 5. Suggested focus for the reviewer

1. The trust boundary on resume: can any content of `.partial` / `.partial.map` make `fetch`
   accept bytes that were not hash-verified, or write outside `--out`?
2. `VolumeSet::validate` and the descriptor path: are all size, count and name limits enforced
   before allocation, in every entry point (`inspect`, `join`, `repair`, `fetch`, `serve`)?
3. The pinning verifiers in `tls.rs`: does anything other than the fingerprint influence
   acceptance; are TLS 1.2 and renegotiation really unreachable; what does a wrong
   `--peer-id` leak?
4. The gate in `transfer.rs`: counting, token buckets and the connection time budget under
   concurrent connections, including the TLS-mode silent close and the client's back-off.
5. Error handling around partial writes and renames in `fetch`, `join` and `repair`: what state
   is left after an interruption at every step, and does the next run recover from it?
6. Anything in the documents that the code does not actually do.

## 6. Reporting

Until the repository is public, findings go directly to the founder through the channel agreed
for the review; afterwards `SECURITY.md` applies (private vulnerability reporting). Please cite
the tag and file paths, and separate what was demonstrated from what was suspected.
