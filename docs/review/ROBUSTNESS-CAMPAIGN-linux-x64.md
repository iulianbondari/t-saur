# Robustness campaign on Linux x64, 300 s per target

A longer run of the campaign in `tsaur/crates/tsaur-core/tests/robustness.rs` than the 60 s per
target recorded by the release gate (`RC1-VERIFICATION-REPORT-linux-x64.md`). Same test binary,
same mutation engine (bit flips, byte sets, zeroed / deleted / inserted / duplicated ranges,
truncation, bursts of these), same seeds: only the time budget per target differs. This is the
producer's own check, run with AI assistance in a cloud container; it is not an independent review.

| Field | Value |
|---|---|
| commit under test | `a8ef56e51ff30b167cc1c70893db5373b6da5a3e` (main after pull request #1; archive code = imported snapshot plus lint-only changes) |
| platform | Linux 6.18.44 x86_64, glibc 2.39, 4 logical CPUs, shared cloud container |
| toolchain | rustc 1.94.1 (e408947bf 2026-03-25), cargo 1.94.1 |
| profile | release |
| started / finished (UTC, 2026-09-25) | 06:34:45 / 07:14:47 (test binary: 2401.22 s) |
| result | `test result: ok. 8 passed; 0 failed`, exit code 0 |
| findings saved under `target/robustness-findings/` | none (the directory was not created: no input panicked) |

Command, from `tsaur/`:

```
TSAUR_ROBUSTNESS_SECONDS=300 cargo test --release --test robustness -- --nocapture --test-threads=1
```

## Results per target

Every target must return an error or a verified result for every mutated input: never a panic,
never a file outside its destination, never unverified bytes presented as content. The outcome
labels are the ones the test prints.

| Target (test) | Iterations in 300 s | Outcomes |
|---|---:|---|
| archive reader: open, list, verify, extract, inspect (`archive_reader_survives_damaged_archives_and_never_returns_wrong_bytes`) | 2,183,371 | opened, verified, restored bit-exact 376; refused at open 2,182,995 |
| extraction destination (`extraction_destination_is_never_escaped_by_any_damage`) | 2,304,584 | checked 405; refused at open 2,304,179 |
| transfer replies as seen by `fetch` (`fetch_survives_damaged_replies_and_never_writes_unverified_volumes`) | 521 | fetch errored 521 |
| manifest and chunk index decoding (`manifest_and_chunk_index_decoding_never_panic`) | 83,885,421 | chunk index decoded 5,292,733; manifest decoded 3,321,836; not decodable 75,270,852 |
| resume files (`resume_files_can_be_arbitrarily_damaged_without_ever_yielding_wrong_volumes`) | 1,350 | resumed and joined bit-exact 1,350 |
| request line on a live server (`server_survives_damaged_request_lines_and_still_serves_afterwards`) | 5,964 | refused 5,764; served 200 |
| volume descriptor: CBOR decode and validation (`volume_descriptor_decoding_and_validation_never_panic`) | 203,282,696 | decoded, invalid 558,228; decoded, valid 22,789,483; not CBOR 179,934,985 |
| `.tsrv` volumes: inspect, join, repair (`volume_tools_survive_damaged_volumes_and_never_join_wrong_bytes`) | 365,439 | joined bit-exact 282,609; set seen, join refused 82,830 |

For comparison, the 60 s run of the release gate on the same machine and commit, earlier the same
day, gave: archive reader 443,226; extraction destination 459,018; fetch replies 111; manifest and
chunk index 16,949,693; resume files 269; request line 1,194; descriptor 43,821,256; volumes
77,531. The 300 s counts are 4.6 to 5.0 times the 60 s counts on every target, as expected from the
budget (the network-bound targets, fetch replies, request line and resume files, scale like the
CPU-bound ones).

## Notes

* The iteration counts are per target, single-threaded, and depend on the machine; they are
  recorded so that a later run can be compared, not as a benchmark.
* The first target (archive reader) overlapped for about five seconds with a duplicate instance
  of the same campaign that had been started by mistake and was killed at 06:34:50 UTC. The
  overlap cost that target some CPU time; it did not change the inputs, the seeds or the checks.
* Temporary directories left in the system temp directory by the test suites (including the
  killed duplicate) were removed afterwards; nothing was written inside the repository.
