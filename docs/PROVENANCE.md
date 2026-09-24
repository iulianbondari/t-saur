# Provenance of the published repository

The public repository `iulianbondari/t-saur` starts from a **verified snapshot** of a private
development repository, not from its history. This page records what the snapshot is, why the
history stays private, and what was checked before publication.

## The snapshot

| Item | Value |
|---|---|
| source | the private development repository of Iulian Bondari, tag `v1.0.0-rc.1` = commit `9e0fe7957edc44b2bb32aec5962c7ddbfa7bf137` (24 September 2026), plus the publication-preparation changes listed below (documents and tooling; no change to the archive code or format) |
| source archive of the tag | `tsaur-1.0.0-rc.1-src.zip`, SHA-256 `b5d25a6460921d8a5ef09175b37ac19c1daaa1b4918e52ff8cac355122eabf0c` (176 files), kept by the maintainer |
| verification of the tag | `docs/review/RC1-VERIFICATION-REPORT.md` (all producer checks passed on Windows 11 x64) |
| import commit | the first commit of this repository; its hash and the SHA-256 manifest of every imported file are in the import report the maintainer keeps with the source archive |

Publication-preparation changes after the tag: community files (`CODE_OF_CONDUCT.md`,
`AUTHORS.md`, `CITATION.cff`, `docs/INSTALL.md`, `docs/PROVENANCE.md`,
`benchmarks/CORPUS-LICENSES.md`, issue and pull-request templates, the draft-release workflow),
neutral copyright lines in the license files, README wording for a public repository, absolute
local paths replaced by relative ones in reports and tool output, and the removal of the Romanian
draft notes from the published tree (the English versions in `docs/research/` are the reference).

## Why the history is not published

* Every commit of the development repository carries a personal e-mail address as author and
  committer identity, which the maintainer does not want in a public history.
* A few committed reports and docstrings contained absolute paths of the development machine.
* The Romanian working drafts of the research notes are not part of the English-only public tree.

Rewriting history to fix these would produce a history nobody has reviewed. A single verified
import commit is simpler to audit: the source archive of the tag and this page tie it to the
development state, and the golden archives in `tsaur/crates/tsaur-core/tests/golden/` tie the
format to that state independently of any git metadata.

## What was checked before publication

* No private keys, certificates, tokens or passwords in the tree or in the history (the only
  key file, `tests/golden/sign.key`, is a published test vector: an Ed25519 seed used to sign the
  golden archive `encrypted.tsr`, whose password is `golden`; it protects nothing).
* No build outputs, no generated corpora from third-party installations (`corpus_real`,
  `corpus_binary`, `corpus_arm64` are built locally and were never committed), no files from
  other projects.
* Third-party texts in the benchmark corpus are listed with their licenses in
  `benchmarks/CORPUS-LICENSES.md`; Rust dependencies in `THIRD-PARTY-NOTICES.md`; the LGPL
  component and the distribution decision in `docs/DISTRIBUTION-POLICY.md`.
* The name T-saur was checked for obvious conflicts in registries and package indexes during the
  naming round (`docs/research/05b-naming-round2-tsaur.md`); that is not a trademark clearance.

## What remains open

* **Copyright holder.** The code and documentation were produced with substantial AI assistance
  under the maintainer's direction (`AUTHORS.md`). Whether, and where, such material carries
  copyright, and who holds it, is not settled; the license files therefore name "the T-saur
  contributors" instead of a person until the maintainer settles the question, if necessary with
  legal advice. The license grant (Apache-2.0 OR MIT) is the maintainer's intent in every case.
* **Trademark.** A formal search (for example TMview and the WIPO Global Brand Database) has not
  been done; a search without hits would not be a legal clearance either.
* **Independent review, second operating system, LGPL reading**: see `ROADMAP.md`, "v1.0 gate".
