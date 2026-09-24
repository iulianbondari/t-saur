# Release candidate verification report (RC1)

These are the **producer's own checks** of the release candidate, run by `tools/release_gate.py`
(machine-specific paths shortened to repository-relative ones for publication);
they are not an independent review (`docs/review/REVIEW-PACKAGE.md` describes that separate step).

| Field | Value |
|---|---|
| date | 2026-09-24 21:27 |
| commit under test | `0ce740696ec25e16ce5b759d8a310944277b70a3` (working tree clean) |
| platform | Windows-11-10.0.26200-SP0, AMD64, 24 logical CPUs |
| toolchain | rustc 1.93.1 (01f6ddf75 2026-02-11); cargo 1.93.1 (083ac5135 2025-12-15) |
| binaries | full `tsaur\target\release\tsaur.exe`, lite `tsaur\target-nolepton\release\tsaur.exe` |
| golden archives | 18 golden files listed, 0 mismatching |
| overall | **all steps passed** |

## Steps

### rustfmt

`cargo fmt --all -- --check` (in `tsaur`)

Exit code 0, 0.4 s

### clippy, default build

`cargo clippy --all-targets -- -D warnings` (in `tsaur`)

Exit code 0, 1.4 s

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.13s
```

### clippy, lite build

`cargo clippy --all-targets --no-default-features --target-dir target-nolepton -- -D warnings` (in `tsaur`)

Exit code 0, 1.3 s

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.09s
```

### release build, full

`cargo build --release` (in `tsaur`)

Exit code 0, 0.3 s

```
    Finished `release` profile [optimized] target(s) in 0.24s
```

### release build, lite

`cargo build --release --no-default-features --target-dir target-nolepton` (in `tsaur`)

Exit code 0, 0.4 s

```
    Finished `release` profile [optimized] target(s) in 0.28s
```

### tests, default build

`cargo test --no-fail-fast` (in `tsaur`)

Exit code 0, 47.6 s

```
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.38s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.68s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.88s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.70s
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 13.43s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.81s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.76s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.79s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.79s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.83s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src\main.rs (target\debug\deps\tsaur-449bcaa73b7df6f0.exe)
     Running tests\cli.rs (target\debug\deps\cli-26c1a561c7576674.exe)
     Running unittests src\lib.rs (target\debug\deps\tsaur_core-09e6cf410381901e.exe)
     Running tests\golden.rs (target\debug\deps\golden-5785cd4174a82f59.exe)
     Running tests\robustness.rs (target\debug\deps\robustness-756e85356a86a4f4.exe)
     Running tests\roundtrip.rs (target\debug\deps\roundtrip-02072e1b0e74a84a.exe)
     Running tests\transfer.rs (target\debug\deps\transfer-5b3c375bc072d5f4.exe)
     Running tests\transfer_limits.rs (target\debug\deps\transfer_limits-65ac3332cbea8990.exe)
     Running tests\transfer_resume.rs (target\debug\deps\transfer_resume-db697540509b47ac.exe)
     Running tests\transfer_tls.rs (target\debug\deps\transfer_tls-8acb38c29ca2c241.exe)
     Running tests\views.rs (target\debug\deps\views-dd4ce6e3e6ed01df.exe)
     Running tests\volumes.rs (target\debug\deps\volumes-e668cde5e1a59806.exe)
```

### tests, lite build

`cargo test --no-fail-fast --no-default-features --target-dir target-nolepton` (in `tsaur`)

Exit code 0, 47.8 s

```
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.36s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.70s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.09s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.69s
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 13.54s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.89s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.75s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.78s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.73s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.85s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src\main.rs (target-nolepton\debug\deps\tsaur-909c0e03df7d2280.exe)
     Running tests\cli.rs (target-nolepton\debug\deps\cli-b0bce0af05603eaf.exe)
     Running unittests src\lib.rs (target-nolepton\debug\deps\tsaur_core-9e88b57222936f86.exe)
     Running tests\golden.rs (target-nolepton\debug\deps\golden-a3f3b154f8fa0976.exe)
     Running tests\robustness.rs (target-nolepton\debug\deps\robustness-3a71914d96da50ec.exe)
     Running tests\roundtrip.rs (target-nolepton\debug\deps\roundtrip-c8bce2a82707461a.exe)
     Running tests\transfer.rs (target-nolepton\debug\deps\transfer-b2ecac9850cba576.exe)
     Running tests\transfer_limits.rs (target-nolepton\debug\deps\transfer_limits-a308ab6edf939714.exe)
     Running tests\transfer_resume.rs (target-nolepton\debug\deps\transfer_resume-919fe15b234e04e5.exe)
     Running tests\transfer_tls.rs (target-nolepton\debug\deps\transfer_tls-7e4be14563619930.exe)
     Running tests\views.rs (target-nolepton\debug\deps\views-7bd507a26f89f5ff.exe)
     Running tests\volumes.rs (target-nolepton\debug\deps\volumes-c139b23eef1e76f5.exe)
```

### robustness campaign, 60 s per target, release profile

`cargo test --release --test robustness -- --nocapture --test-threads=1` (in `tsaur`), env `{'TSAUR_ROBUSTNESS_SECONDS': '60'}`

Exit code 0, 483.1 s

```
test archive_reader_survives_damaged_archives_and_never_returns_wrong_bytes ... robustness archive-reader: 193719 iterations in 60.0 s, outcomes {"opened, verified, restored bit-exact": 30, "refused at open": 193689}
test extraction_destination_is_never_escaped_by_any_damage ... robustness extract-destination: 166287 iterations in 60.0 s, outcomes {"checked": 28, "refused at open": 166259}
test fetch_survives_damaged_replies_and_never_writes_unverified_volumes ... robustness fetch-replies: 111 iterations in 62.0 s, outcomes {"fetch errored": 111}
test manifest_and_chunk_index_decoding_never_panic ... robustness manifest-and-index: 23308730 iterations in 60.0 s, outcomes {"chunk index decoded": 1471299, "manifest decoded": 923300, "not decodable": 20914131}
test resume_files_can_be_arbitrarily_damaged_without_ever_yielding_wrong_volumes ... robustness resume-files: 269 iterations in 60.2 s, outcomes {"resumed and joined bit-exact": 269}
test server_survives_damaged_request_lines_and_still_serves_afterwards ... robustness request-line: 1189 iterations in 60.0 s, outcomes {"refused": 1146, "served": 43}
test volume_descriptor_decoding_and_validation_never_panic ... robustness descriptor: 55157766 iterations in 60.0 s, outcomes {"decoded, invalid": 151894, "decoded, valid": 6182807, "not cbor": 48823065}
test volume_tools_survive_damaged_volumes_and_never_join_wrong_bytes ... robustness volumes: 13664 iterations in 60.0 s, outcomes {"joined bit-exact": 10566, "set seen, join refused": 3098}
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 482.84s
```

### cross-build compatibility of the golden archives

`python tools\compat_check.py` (in `.`)

Exit code 0, 4.4 s

```
Readers: full = `tsaur 1.0.0-rc.1` (tsaur\target\release\tsaur.exe), lite = `tsaur 1.0.0-rc.1` (tsaur\target-nolepton\release\tsaur.exe)
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

`python tools\check_guide.py --tsaur tsaur\target\release\tsaur.exe` (in `.`)

Exit code 0, 3.9 s

```
   | photo.bin | 100000 | raw | 28572 | 9a288fb79ffa |
   A demo.tsr (3 entries)  B demo-rebuilt.tsr (3 entries)  IDENTICAL
     unchanged 3
   chunks of B: 4 total, 4 already in A (145630 bytes), 0 new (0 bytes = 0.0% of B) -> `pack --ref` would store only the new ones
-- block at line 182: ok
10 block(s) ran, 2 skipped, binary tsaur\target\release\tsaur.exe
```

### guide executed with the lite binary

`python tools\check_guide.py --tsaur tsaur\target-nolepton\release\tsaur.exe` (in `.`)

Exit code 0, 3.8 s

```
   | photo.bin | 100000 | raw | 28572 | 64e40b0193b6 |
   A demo.tsr (3 entries)  B demo-rebuilt.tsr (3 entries)  IDENTICAL
     unchanged 3
   chunks of B: 4 total, 4 already in A (145630 bytes), 0 new (0 bytes = 0.0% of B) -> `pack --ref` would store only the new ones
-- block at line 182: ok
10 block(s) ran, 2 skipped, binary tsaur\target-nolepton\release\tsaur.exe
```

### cargo package --list (core)

`cargo package --list -p tsaur-core` (in `tsaur`)

Exit code 0, 0.2 s

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

Exit code 0, 4.7 s

```
   Packaging tsaur-core v1.0.0-rc.1 (tsaur\crates\tsaur-core)
    Packaged 52 files, 883.3KiB (290.5KiB compressed)
   Verifying tsaur-core v1.0.0-rc.1 (tsaur\crates\tsaur-core)
   Compiling tsaur-core v1.0.0-rc.1 (tsaur\target\package\tsaur-core-1.0.0-rc.1)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.66s
```

### cargo publish --dry-run (core; recorded, nothing is published)

`cargo publish --dry-run -p tsaur-core --allow-dirty` (in `tsaur`)

Exit code 0, 2.7 s (informational step)

```
   Packaging tsaur-core v1.0.0-rc.1 (tsaur\crates\tsaur-core)
    Packaged 52 files, 883.3KiB (290.5KiB compressed)
   Verifying tsaur-core v1.0.0-rc.1 (tsaur\crates\tsaur-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.67s
   Uploading tsaur-core v1.0.0-rc.1 (tsaur\crates\tsaur-core)
warning: aborting upload due to dry run
```

### cargo publish --dry-run (cli; expected to fail until tsaur-core is published)

`cargo publish --dry-run -p tsaur --allow-dirty` (in `tsaur`)

Exit code 101, 1.9 s (informational step)

```
   Packaging tsaur v1.0.0-rc.1 (tsaur\crates\tsaur-cli)
error: failed to prepare local package for uploading
```

### release packages built and checked from a clean directory

`python tools\package.py` (in `.`)

Exit code 0, 8.7 s

```
tsaur-1.0.0-rc.1-windows-x64-lite.zip: 3,965,013 bytes, sha256 a8bbdffa5967287370f6f77a53fb1139263d6dca4588819dc3ae09bce963a928, clean-directory check ok (tsaur 1.0.0-rc.1; guide: 10 block(s) ran, 2 skipped, binary <temp>)
tsaur-1.0.0-rc.1-windows-x64-full.zip: 4,233,932 bytes, sha256 728126ab006407c395c7abb641517717c7584b4749f4fe3cabe4a5a4f390970c, clean-directory check ok (tsaur 1.0.0-rc.1; guide: 10 block(s) ran, 2 skipped, binary <temp>)
written: dist\SHA256SUMS, dist\PACKAGES.md
```

