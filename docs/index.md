---
title: T-saur documentation
---

# T-saur (`.tsr`)

A free, open-source archive format and command-line archiver, written in Rust, for files that
people and AI agents need to list, search, read and verify without unpacking them. Archives are
content-addressed (BLAKE3) and deterministic; encryption with post-quantum recipients, signatures,
offline N + M recovery volumes and a Model Context Protocol server are optional parts of the same
binary. Format v1 is frozen; the software is at 1.0.0-rc.1, on crates.io (`cargo install tsaur`;
library `tsaur-core`, <https://docs.rs/tsaur-core>), with no tagged release or binary package
yet. Apache-2.0 OR MIT, no account, no service.

Repository and README: <https://github.com/iulianbondari/t-saur>

## Start here

* [Installing](INSTALL.md): `cargo install tsaur`, or building from source (Rust 1.87 or newer); binary packages once tagged.
* [User guide](GUIDE.md): first archive, encryption, volume sets, transfers, reading without
  extracting, exit codes. Every command in it is executed before a release.
* [The v1.0 contract](V1-CONTRACT.md): the nine promises, what is outside v1.0, the reader's
  limits, verified platforms, what "verified" means.
* [Design: what an AI agent needs from an archiver](DESIGN-AGENT-FIRST.md).

## Format and security

* [Format specification v1.0](spec/TSAUR-FORMAT-SPEC-v1.0.md) (CC-BY-4.0; frozen).
* [Security policy](https://github.com/iulianbondari/t-saur/blob/main/SECURITY.md): what the
  reader guarantees, what the volume exchange guarantees, how to report privately.
* [Distribution policy](DISTRIBUTION-POLICY.md): lite and full builds, the LGPL component.
* [Volume sets](design/VOLUME-SETS.md) and the [volume trust contract](design/VOLUME-TRUST.md).

## Verification and provenance

* [Review package for an independent evaluator](review/REVIEW-PACKAGE.md) and the producer's
  reports: [Windows](review/RC1-VERIFICATION-REPORT.md),
  [Linux](review/RC1-VERIFICATION-REPORT-linux-x64.md),
  [robustness campaign](review/ROBUSTNESS-CAMPAIGN-linux-x64.md). No independent review has
  taken place yet.
* [Provenance of the published repository](PROVENANCE.md) and the
  [release process](RELEASE-PROCESS.md).
* [Two-device benchmark plan](design/TWO-DEVICE-BENCHMARK-PLAN.md).

## Project

[Roadmap](https://github.com/iulianbondari/t-saur/blob/main/ROADMAP.md) ·
[Changelog](https://github.com/iulianbondari/t-saur/blob/main/CHANGELOG.md) ·
[Authors and provenance](https://github.com/iulianbondari/t-saur/blob/main/AUTHORS.md) ·
[Contributing](https://github.com/iulianbondari/t-saur/blob/main/CONTRIBUTING.md) ·
[Third-party notices](https://github.com/iulianbondari/t-saur/blob/main/THIRD-PARTY-NOTICES.md)
