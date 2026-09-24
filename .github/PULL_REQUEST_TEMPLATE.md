## What this changes

<!-- One paragraph: the problem, the change, what a user sees differently. -->

## Format impact

- [ ] No change to the `.tsr` / `.tsrv` bytes (the golden tests in `tsaur/crates/tsaur-core/tests/golden.rs` still pass unchanged)
- [ ] Changes the writer's output: I updated `tsaur_core::WRITER`, regenerated the golden archives with `tests/golden/make_golden.py`, added compatibility tests and a CHANGELOG entry (see `CONTRIBUTING.md`, "Format freeze")

## Checks run locally

- [ ] `cargo test` (default build)
- [ ] `cargo test --no-default-features --target-dir target-nolepton` (lite build)
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`
- [ ] `python tools/check_guide.py` if the guide or a CLI command changed

## Notes for the reviewer

<!-- Anything not obvious from the diff: trade-offs, measurements (with commands), what was not tested. -->

By opening this pull request I confirm that I have the right to contribute this work under Apache-2.0 OR MIT and that I have stated any AI assistance used, as `CONTRIBUTING.md` asks.
