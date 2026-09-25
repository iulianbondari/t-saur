# Release candidate verification report (RC1)

These are the **producer's own checks** of the release candidate, run by `tools/release_gate.py`;
they are not an independent review (`docs/review/REVIEW-PACKAGE.md` describes that separate step).

| Field | Value |
|---|---|
| date | 2026-09-25 06:33 |
| commit under test | `a8ef56e51ff30b167cc1c70893db5373b6da5a3e` (working tree clean) |
| platform | Linux-6.18.44-fc-v37-x86_64-with-glibc2.39, x86_64, 4 logical CPUs |
| toolchain | rustc 1.94.1 (e408947bf 2026-03-25); cargo 1.94.1 (29ea6fb6a 2026-03-24) |
| binaries | full `tsaur/target/release/tsaur`, lite `tsaur/target-nolepton/release/tsaur` |
| golden archives | 18 golden files listed, 0 mismatching |
| overall | **all steps passed** |

## Steps

### rustfmt

`cargo fmt --all -- --check` (in `tsaur`)

Exit code 0, 0.4 s

### clippy, default build

`cargo clippy --all-targets -- -D warnings` (in `tsaur`)

Exit code 0, 0.3 s

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.22s
```

### clippy, lite build

`cargo clippy --all-targets --no-default-features --target-dir target-nolepton -- -D warnings` (in `tsaur`)

Exit code 0, 0.2 s

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s
```

### release build, full

`cargo build --release` (in `tsaur`)

Exit code 0, 0.2 s

```
    Finished `release` profile [optimized] target(s) in 0.15s
```

### release build, lite

`cargo build --release --no-default-features --target-dir target-nolepton` (in `tsaur`)

Exit code 0, 0.2 s

```
    Finished `release` profile [optimized] target(s) in 0.15s
```

### tests, default build

`cargo test --no-fail-fast` (in `tsaur`)

Exit code 0, 118.3 s

```
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.12s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.16s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.20s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.40s
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 23.09s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.18s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.30s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.73s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.70s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.71s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/main.rs (target/debug/deps/tsaur-015aceaee74ba7d4)
     Running tests/cli.rs (target/debug/deps/cli-d020c34aba0fde82)
     Running unittests src/lib.rs (target/debug/deps/tsaur_core-780943217ebd90d8)
     Running tests/golden.rs (target/debug/deps/golden-681c06ec5bcf1a48)
     Running tests/robustness.rs (target/debug/deps/robustness-207848b9451bbed1)
     Running tests/roundtrip.rs (target/debug/deps/roundtrip-fa7fbccb42243a96)
     Running tests/transfer.rs (target/debug/deps/transfer-59e8dede8dda0365)
     Running tests/transfer_limits.rs (target/debug/deps/transfer_limits-1b2801446a0e7181)
     Running tests/transfer_resume.rs (target/debug/deps/transfer_resume-28645645478e5d4f)
     Running tests/transfer_tls.rs (target/debug/deps/transfer_tls-24e32b95f077d4d6)
     Running tests/views.rs (target/debug/deps/views-2c4bf5ad81b61904)
     Running tests/volumes.rs (target/debug/deps/volumes-f458554a28fb9528)
```

### tests, lite build

`cargo test --no-fail-fast --no-default-features --target-dir target-nolepton` (in `tsaur`)

Exit code 0, 117.4 s

```
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.10s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.08s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.02s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.43s
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 22.91s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.14s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.32s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.83s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.68s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.48s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/main.rs (target-nolepton/debug/deps/tsaur-14b6110b1cd6dff8)
     Running tests/cli.rs (target-nolepton/debug/deps/cli-0a0dc82c7d8f9d6a)
     Running unittests src/lib.rs (target-nolepton/debug/deps/tsaur_core-835647d8b2be11ea)
     Running tests/golden.rs (target-nolepton/debug/deps/golden-253cdff484760814)
     Running tests/robustness.rs (target-nolepton/debug/deps/robustness-47a21dcd26da181b)
     Running tests/roundtrip.rs (target-nolepton/debug/deps/roundtrip-e8fc476a3c286589)
     Running tests/transfer.rs (target-nolepton/debug/deps/transfer-0fac95a2ab232d59)
     Running tests/transfer_limits.rs (target-nolepton/debug/deps/transfer_limits-e39ebe0ff0c77379)
     Running tests/transfer_resume.rs (target-nolepton/debug/deps/transfer_resume-c4e2a1a88f24a3a9)
     Running tests/transfer_tls.rs (target-nolepton/debug/deps/transfer_tls-fb5fd4303ba38079)
     Running tests/views.rs (target-nolepton/debug/deps/views-76cec853bb1e7dab)
     Running tests/volumes.rs (target-nolepton/debug/deps/volumes-2eb5abbb3f9beee8)
```

### robustness campaign, 60 s per target, release profile

`cargo test --release --test robustness -- --nocapture --test-threads=1` (in `tsaur`), env `{'TSAUR_ROBUSTNESS_SECONDS': '60'}`

Exit code 0, 525.9 s

```
test archive_reader_survives_damaged_archives_and_never_returns_wrong_bytes ... robustness archive-reader: 443226 iterations in 60.0 s, outcomes {"opened, verified, restored bit-exact": 81, "refused at open": 443145}
test extraction_destination_is_never_escaped_by_any_damage ... robustness extract-destination: 459018 iterations in 60.0 s, outcomes {"checked": 69, "refused at open": 458949}
test fetch_survives_damaged_replies_and_never_writes_unverified_volumes ... robustness fetch-replies: 111 iterations in 61.4 s, outcomes {"fetch errored": 111}
test manifest_and_chunk_index_decoding_never_panic ... robustness manifest-and-index: 16949693 iterations in 60.0 s, outcomes {"chunk index decoded": 1070485, "manifest decoded": 671967, "not decodable": 15207241}
test resume_files_can_be_arbitrarily_damaged_without_ever_yielding_wrong_volumes ... robustness resume-files: 269 iterations in 60.0 s, outcomes {"resumed and joined bit-exact": 269}
test server_survives_damaged_request_lines_and_still_serves_afterwards ... robustness request-line: 1194 iterations in 60.0 s, outcomes {"refused": 1151, "served": 43}
test volume_descriptor_decoding_and_validation_never_panic ... robustness descriptor: 43821256 iterations in 60.0 s, outcomes {"decoded, invalid": 120551, "decoded, valid": 4910349, "not cbor": 38790356}
test volume_tools_survive_damaged_volumes_and_never_join_wrong_bytes ... robustness volumes: 77531 iterations in 60.0 s, outcomes {"joined bit-exact": 59885, "set seen, join refused": 17646}
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 482.22s
```

### cross-build compatibility of the golden archives

`python tools/compat_check.py` (in `.`)

Exit code 0, 6.9 s

```
Readers: full = `tsaur 1.0.0-rc.1` (tsaur/target/release/tsaur), lite = `tsaur 1.0.0-rc.1` (tsaur/target-nolepton/release/tsaur)
| Archive | Reader | `list` | `info` (`requires`) | `verify` | `unpack` | Restored bit-exact |
|---|---|:---:|:---:|:---:|:---:|:---:|
| `full-default.tsr` | full | ok | ok (lepton) | ok | ok | yes |
| `full-default.tsr` | lite | ok | ok (lepton) | exit 2 | exit 7 | n/a |
| `lite-default.tsr` | full | ok | ok (–) | ok | ok | yes |
| `lite-default.tsr` | lite | ok | ok (–) | ok | ok | yes |
| `canonical.tsr` | full | ok | ok (–) | ok | ok | yes |
| `canonical.tsr` | lite | ok | ok (–) | ok | ok | yes |
| `incremental.tsr` | full | ok | ok (–) | ok | ok | yes |
| `incremental.tsr` | lite | ok | ok (–) | ok | ok | yes |
| `encrypted.tsr` | full | ok | ok (–) | ok | ok | yes |
| `encrypted.tsr` | lite | ok | ok (–) | ok | ok | yes |
| `volumes-2p1/` (split by the full build) | full `volumes join` | – | – | – | ok | yes |
| `volumes-2p1/` (split by the full build) | lite `volumes join` | – | – | – | ok | yes |
all outcomes as expected
```

### guide executed with the full binary

`python tools/check_guide.py --tsaur tsaur/target/release/tsaur` (in `.`)

Exit code 0, 2.3 s

```
   | photo.bin | 100000 | raw | 28572 | 196f10450db7 |
   A demo.tsr (3 entries)  B demo-rebuilt.tsr (3 entries)  IDENTICAL
     unchanged 3
   chunks of B: 4 total, 4 already in A (145630 bytes), 0 new (0 bytes = 0.0% of B) -> `pack --ref` would store only the new ones
-- block at line 182: ok
10 block(s) ran, 2 skipped, binary tsaur/target/release/tsaur
```

### guide executed with the lite binary

`python tools/check_guide.py --tsaur tsaur/target-nolepton/release/tsaur` (in `.`)

Exit code 0, 2.8 s

```
   | photo.bin | 100000 | raw | 28572 | 148a1423f680 |
   A demo.tsr (3 entries)  B demo-rebuilt.tsr (3 entries)  IDENTICAL
     unchanged 3
   chunks of B: 4 total, 4 already in A (145630 bytes), 0 new (0 bytes = 0.0% of B) -> `pack --ref` would store only the new ones
-- block at line 182: ok
10 block(s) ran, 2 skipped, binary tsaur/target-nolepton/release/tsaur
```

### cargo package --list (core)

`cargo package --list -p tsaur-core` (in `tsaur`)

Exit code 0, 0.1 s

```
src/format.rs
src/lib.rs
src/manifest.rs
src/pack.rs
src/paths.rs
src/pieces.rs
src/read.rs
src/tls.rs
src/transfer.rs
src/volumes.rs
tests/golden/MANIFEST.sha256
tests/golden/base.tsr
tests/golden/canonical.tsr
tests/golden/encrypted.tsr
tests/golden/full-default.tsr
tests/golden/incremental.tsr
tests/golden/inputs/notes.md
tests/golden/inputs/paper.pdf
tests/golden/inputs/photo.jpg
tests/golden/inputs/random.bin
tests/golden/inputs/report.docx
tests/golden/inputs/sub/notes-v2.md
tests/golden/lite-default.tsr
tests/golden/make_golden.py
tests/golden/make_inputs.py
tests/golden/sign.key
tests/golden/volumes-2p0/full-default.tsr.v01.tsrv
tests/golden/volumes-2p0/full-default.tsr.v02.tsrv
tests/golden/volumes-2p1/full-default.tsr.v01.tsrv
tests/golden/volumes-2p1/full-default.tsr.v02.tsrv
tests/golden/volumes-2p1/full-default.tsr.v03.tsrv
tests/golden.rs
tests/robustness.rs
tests/roundtrip.rs
tests/transfer.rs
tests/transfer_limits.rs
tests/transfer_resume.rs
tests/transfer_tls.rs
tests/views.rs
tests/volumes.rs
```

### cargo package --list (cli)

`cargo package --list -p tsaur` (in `tsaur`)

Exit code 0, 0.1 s

```
.cargo_vcs_info.json
Cargo.lock
Cargo.toml
Cargo.toml.orig
README.md
src/main.rs
src/mcp.rs
src/ops.rs
tests/cli.rs
```

### cargo package (core, builds the packaged crate)

`cargo package -p tsaur-core --allow-dirty` (in `tsaur`)

Exit code 0, 11.8 s

```
   Verifying tsaur-core v1.0.0-rc.1 (tsaur/crates/tsaur-core)
   Compiling simd-adler32 v0.3.10
   Compiling base64 v0.22.1
   Compiling miniz_oxide v0.9.1
   Compiling pem v3.0.6
   Compiling rcgen v0.13.2
   Compiling flate2 v1.1.10
   Compiling lopdf v0.42.0
   Compiling lepton_jpeg v0.5.8
   Compiling pdf-extract v0.12.1
   Compiling tsaur-core v1.0.0-rc.1 (tsaur/target/package/tsaur-core-1.0.0-rc.1)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.75s
```

### cargo publish --dry-run (core; recorded, nothing is published)

`cargo publish --dry-run -p tsaur-core --allow-dirty` (in `tsaur`)

Exit code 0, 1.7 s (informational step)

```
   Packaging tsaur-core v1.0.0-rc.1 (tsaur/crates/tsaur-core)
    Packaged 52 files, 877.5KiB (289.6KiB compressed)
   Verifying tsaur-core v1.0.0-rc.1 (tsaur/crates/tsaur-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.70s
   Uploading tsaur-core v1.0.0-rc.1 (tsaur/crates/tsaur-core)
warning: aborting upload due to dry run
```

### cargo publish --dry-run (cli; expected to fail until tsaur-core is published)

`cargo publish --dry-run -p tsaur --allow-dirty` (in `tsaur`)

Exit code 101, 0.9 s (informational step)

```
   Packaging tsaur v1.0.0-rc.1 (tsaur/crates/tsaur-cli)
error: failed to prepare local package for uploading
```

### release packages built and checked from a clean directory

`python tools/package.py` (in `.`)

Exit code 0, 6.8 s

```
tsaur-1.0.0-rc.1-linux-x64-lite.zip: 4,253,649 bytes, sha256 d8768174d2b271223fb18036af24bd65329aadd34b1d9ba68490a309daa5fd31, clean-directory check ok (tsaur 1.0.0-rc.1; guide: 10 block(s) ran, 2 skipped, binary tsaur)
tsaur-1.0.0-rc.1-linux-x64-full.zip: 4,529,753 bytes, sha256 e705172cd251fc563648fae2beeecb2939e23b347dba9da992cb50501a1b37bb, clean-directory check ok (tsaur 1.0.0-rc.1; guide: 10 block(s) ran, 2 skipped, binary tsaur)
written: dist/SHA256SUMS, dist/PACKAGES.md
```

