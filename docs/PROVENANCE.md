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
| verification on a second operating system | `docs/review/RC1-VERIFICATION-REPORT-linux-x64.md` (all producer checks passed on Linux x64, commit `a8ef56e` of this repository, whose archive code is the imported snapshot plus lint-only changes) |
| import commit | the first commit of this repository: `16eeeec879ecad3c271fb02dcb47aa76a69ea3dc` in the original history, `29bc6727bc3f476a03d7495c22df08af1001760c` after the rewrite of 2026-09-25 (below); its SHA-256 manifest of every imported file is in the import report the maintainer keeps with the source archive |

Publication-preparation changes after the tag: community files (`CODE_OF_CONDUCT.md`,
`AUTHORS.md`, `CITATION.cff`, `docs/INSTALL.md`, `docs/PROVENANCE.md`,
`benchmarks/CORPUS-LICENSES.md`, issue and pull-request templates, the draft-release workflow),
neutral copyright lines in the license files, README wording for a public repository, absolute
local paths replaced by relative ones in reports and tool output, and the removal of the Romanian
draft notes from the published tree.

## Internal material removed from the tree (2026-09-25)

The research notes (`docs/research/`, seven areas plus the naming rounds) and the name-availability
script (`benchmarks/check_names.sh`) were the maintainer's working material for the design; they
were removed from the tree before publication and are kept privately. Their conclusions live in
`docs/DESIGN-AGENT-FIRST.md`, `docs/design/` and the specification. **They are still present in
this repository's history** (the import commit and the commits up to that date), so before the
repository is made public the maintainer either rewrites the history to drop them (for example
with `git filter-repo --path docs/research --path benchmarks/check_names.sh --invert-paths`,
followed by a forced update of `main`, which is acceptable only while the repository is private)
or re-creates the public repository from a clean snapshot. The first way was taken on 2026-09-25;
the next section records it.

## History rewritten before publication (2026-09-25)

Executed once, while the repository was still private, as `docs/RELEASE-PROCESS.md` §A1
describes, with the maintainer's explicit authorisation of the same day. Command, in a fresh
clone with `git-filter-repo` 2.x:

```
git filter-repo --path docs/research --path benchmarks/check_names.sh --invert-paths --mailmap MAILMAP --force
```

| Item | Before | After |
|---|---|---|
| tip of `main` | `7fd480f6ac8a5a2b7aa29e18c4c35f67d9288e36` | `cce66e1c9cfa6368318633001ea1d4b84e958d06` |
| tree of the tip | `a881d31adc428661ac98ead32fe9f74ddcfe4e44` | `a881d31adc428661ac98ead32fe9f74ddcfe4e44` (identical) |
| import commit (root) | `16eeeec879ecad3c271fb02dcb47aa76a69ea3dc` | `29bc6727bc3f476a03d7495c22df08af1001760c` |
| commits on `main` | 43 | 43 |
| commits touching `docs/research/` or `benchmarks/check_names.sh` | 2 | 0 |
| author/committer identities | 4 (two of them addresses the maintainer does not publish) | the maintainer's GitHub no-reply identity on every commit; `GitHub <noreply@github.com>` as committer of the web merges |
| `Co-Authored-By` trailers (AI assistance) | 16 | 16 |

The mailmap mapped the private address used by the maintainer's tooling, and the assistant's
default git identity on six merge commits, to `Iulian Bondari
<76534436+iulianbondari@users.noreply.github.com>`; AI assistance stays stated in `AUTHORS.md` and
in the trailers. Pull requests #1 to #15 keep their pages on GitHub; the commits they list belong
to the old history and are no longer reachable from `main`. The maintainer's private mirror keeps
the old history. Nothing else about the content changed: the tree hash proves it.

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
  naming round (September 2026; no product, company or package of that name was found); that is
  not a trademark clearance.

## What remains open

* **Copyright holder: settled by the maintainer (2026-09-25).** Iulian Bondari conceived and
  directed the project and holds the copyright in the work as published; the license files name
  him, and the grant is Apache-2.0 OR MIT for everyone. The fact that the code and documentation
  were produced with substantial AI assistance under his direction stays recorded in
  `AUTHORS.md` as provenance, because the treatment of AI-assisted material differs between
  jurisdictions; the maintainer has chosen not to seek legal advice on it, and the record lets
  anyone apply the rules of their own jurisdiction. Contributors keep the copyright in their
  contributions (`CONTRIBUTING.md`).
* **Trademark.** The maintainer has decided not to reserve names or to file or search for a
  trademark: T-saur is used only as the name of a free software project. Anyone who needs a
  legal clearance of the name has to obtain it themselves.
* **Independent review, second operating system, LGPL reading**: see `ROADMAP.md`, "v1.0 gate".
