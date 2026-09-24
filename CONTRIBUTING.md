# Contributing to T-saur

Thank you for helping. T-saur is an international, open-source project: **everything in the
repository is written in English** (code, comments, docs, commit messages), and the project must
stay free to use and free to build (no paid services in the default path).

## Getting started

```bash
cd tsaur
cargo build --release
cargo test --release
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The Rust workspace lives in `tsaur/` (`tsaur-core` library + `tsaur` CLI). Benchmarks are in
`benchmarks/`, the format specification in `docs/spec/`, the research corpus in `docs/research/`.

## Ground rules

1. **Tests and clippy stay green.** Every change comes with tests (`crates/tsaur-core/tests/`,
   `crates/tsaur-cli/tests/`, unit tests next to the code). CI runs on Linux, macOS and Windows.
2. **Determinism is a feature.** Identical inputs and options must produce byte-identical archives
   on every platform. If your change alters the output bytes for existing inputs, say so in the PR,
   bump the `generator` string and update the determinism check in `.github/workflows/ci.yml`.
3. **Bit-exact or nothing.** Container awareness (ZIP, PDF, JPEG) may only be used when the
   rebuild is verified bit-exact at pack time; otherwise the raw bytes are stored. Never trade
   fidelity for ratio silently.
4. **The reader parses hostile input.** New fields, codecs or filters must be bounds-checked,
   covered by the corruption test and unable to allocate beyond the documented ceilings.
5. **Format changes go through the spec.** New codec ids, filter ids, section types, stanza types
   or manifest fields are registered in `docs/spec/TSAUR-FORMAT-SPEC-v1.0.md` in the same PR.
   Unknown ids must remain rejected by older readers with a clear error.
6. **Numbers must be reproducible.** Benchmark claims in `README.md` / `benchmarks/RESULTS*.md`
   come from `benchmarks/bench_rust.py` (or a documented script) with the corpus and the exact
   command lines. Lower percentage = smaller archive = better; always say so next to a table.
7. **No secrets, no telemetry.** The CLI never phones home and never writes credentials to disk
   except the key files the user explicitly asks `keygen` to create.
8. **Permissive dependencies only.** New dependencies must be MIT / Apache-2.0 / BSD-class; the one
   LGPL component (`cabac`, via the optional `lepton` feature) is documented in the README, and any
   change to that situation goes through the README and CHANGELOG. Check with `cargo metadata`.

## Adding a codec or a filter

* Codecs are selected per block in `codec::compress_best`; add the id in `codec.rs`, the
  decoder branch in `decompress_inner`, a round-trip unit test and a line in the spec's codec table.
  A codec must be deterministic for a given input and parameters; parameters the decoder needs are
  stored per blob (`BlobRecord.p`).
* Pre-filters (branch converters and similar) use the `BlobRecord.f` byte: low nibble = id, high
  nibble = parameter. They must be exactly invertible; add an adversarial round-trip test like
  `branch_filters_roundtrip_on_adversarial_inputs`.

## Commit messages and attribution

Use a short imperative subject and a body that explains *why*. AI-assisted changes are welcome;
say so in the pull request and keep the `Co-Authored-By:` trailer that the assistant adds, so
that authorship stays honest (the project itself was written with substantial AI assistance, see
`AUTHORS.md`). By opening a pull request you confirm that you have the right to contribute the
work under the project's license; there is no separate contributor agreement.

## License

By contributing you agree that your contribution is licensed under the project's dual license,
**Apache-2.0 OR MIT** (see `LICENSE-APACHE` and `LICENSE-MIT`), at the user's option.

## Format freeze

Format v1 is frozen (`docs/spec/TSAUR-FORMAT-SPEC-v1.0.md` §12). `tests/golden.rs` compares the
writer's output with the golden archives in `tsaur/crates/tsaur-core/tests/golden/`; a change that
alters those bytes is a format event, not a refactoring: it needs a new writer generation string
(`tsaur_core::WRITER`), regenerated golden archives (`tests/golden/make_golden.py`), a CHANGELOG
entry and compatibility tests, and it may never make v1 readers misread an archive.
