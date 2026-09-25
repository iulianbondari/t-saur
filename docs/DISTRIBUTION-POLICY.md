# Distribution policy: builds, formats and what reads what

Status: measured on 2026-09-24 with the two build configurations of the reference implementation
(`tsaur/`), the benchmark corpora and the `tsaur` CLI; the golden-archive matrix of §5 is produced
by `tools/compat_check.py` from the release binaries. Percentages are archive size relative to the
input (lower is better).

## 1. The two builds

| Build | Cargo command | Contains | Creates | Reads |
|---|---|---|---|---|
| **full** (default) | `cargo build --release` | everything, including Lepton JPEG recompression (`lepton_jpeg`); like every build it contains `cabac` (LGPL-3.0-or-later) through `preflate-rs` | every archive kind | every valid archive |
| **lite** | `cargo build --release --no-default-features` | everything except Lepton; still contains `cabac` (LGPL-3.0-or-later) through `preflate-rs` | every archive kind except Lepton segments: JPEG files and `DCTDecode` streams are stored as they are (still bit-exact, just not smaller) | every archive **without** Lepton segments |

Both builds produce byte-identical archives for inputs without JPEG content (checked: the 10-file
benchmark corpus packs to the same 956,731 bytes with either binary). Volume sets (`.tsrv`) treat
the archive as opaque bytes: either build splits, inspects, joins and repairs the volumes of any
archive, including one that only the full build can open afterwards.

## 2. Compatibility matrix (measured)

Archive `corpus_real` (13 real DOCX/PDF/JPEG files, 578,890 bytes):

| Archive created by | Size | Reader | `list` | `info` | `verify` | `unpack` |
|---|---:|---|:---:|:---:|:---:|:---:|
| full (Lepton segments for 3 JPEGs) | 453,116 B (78.3 %) | full | ok | ok | ok | ok, bit-exact |
| full (Lepton segments) | 453,116 B | lite | ok | ok, reports `requires: lepton` | **fails** (exit 2, JPEG entries reported bad) | **fails** (exit 7: "this build has no Lepton support") |
| lite (JPEGs stored raw, 3 container fallbacks) | 505,943 B (87.4 %) | full | ok | ok | ok | ok, bit-exact |
| lite | lite | ok | ok | ok | ok, bit-exact |
| any build, archive without JPEG content (`corpus`, 956,731 B) | identical bytes | full or lite | ok | ok | ok | ok, bit-exact |
| any Lepton archive split into volumes by the full build | – | lite `volumes join` | – | – | – | joined file byte-identical (opening it still needs the full build) |

Every archive records what it needs: `tsaur list --json`, `tsaur info` and `tsaur verify --json`
report `requires` (today only `lepton`) and, when the running build lacks it, `missing_features`,
so a user knows before extracting which binary to use. Entries that are not JPEG-based are readable
by the lite build even inside an archive that contains Lepton segments (`grep`, `read`, `stat` on
those entries work; `unpack` of the whole archive does not).

**What disabling the feature does not do:** it does not make every existing archive portable. An
archive that contains Lepton segments needs a Lepton-capable reader for those entries, whichever
binary created it. Archives that must be readable by the lite build must be created with the lite
build or with `pack --no-container`.

## 3. Public distribution: the v1.0 decision

1. **Source** is the primary distribution: Apache-2.0 OR MIT, complete, with `Cargo.lock` pinning
   every dependency version. Anyone can build either configuration.
2. **The recommended public binary is the lite build**, packaged as `tsaur-<version>-<platform>-lite.zip`.
   Every archive it writes opens in every build. It contains, like every build, the LGPL-3.0
   component `cabac` through `preflate-rs` (see §3.4 and `THIRD-PARTY-NOTICES.md`), so a redistributor
   (someone bundling `tsaur` into a product or an image) takes on no obligation beyond the
   permissive notices. Its cost is measurable and documented: JPEG files and PDF images are
   stored as they are (`corpus_real`: 87.4 % instead of 78.3 %; everything else is identical).
3. **The full build is published alongside**, as `tsaur-<version>-<platform>-full.zip`, for users
   who want Lepton JPEG recompression and accept that archives containing recompressed JPEGs need
   the full build to open (`pack --no-lepton` in the full build writes universal archives). The
   package carries `THIRD-PARTY-NOTICES.md`, `licenses/LGPL-3.0.txt` and `licenses/GPL-3.0.txt`.
4. **LGPL obligations of every binary** (both builds contain `cabac` through `preflate-rs`; until
   2026-09-25 this section wrongly spoke of the full binary only), as the project reads LGPL-3.0 §4
   for a statically linked library: prominent notice (`THIRD-PARTY-NOTICES.md`), the license texts, and the
   Corresponding Application Code in a form that permits relinking, which the complete
   Apache/MIT source with `Cargo.lock` provides (`cargo vendor` reproduces the exact `cabac`
   sources). **This reading has not been confirmed by counsel, and the maintainer decided on
   2026-09-25 not to seek that confirmation.** It remains the material uncertainty of this policy:
   no test can settle it, which is why every package carries the notices, the license texts and a
   pointer to the complete sources. A distributor of either binary who wants certainty should
   obtain their own legal advice; the project does not claim that the question is closed, and
   removing `cabac` from every build is roadmap item 1.1 so that the question disappears.
5. **Packages are built and checked by `tools/package.py`**: each zip is unpacked in a clean
   directory and the packaged binary runs every command of `docs/GUIDE.md`; sizes and SHA-256
   values are written to `dist/SHA256SUMS` and `dist/PACKAGES.md`. Nothing is uploaded by the
   tool.
4. **No superiority or security claims** in release material beyond what `benchmarks/RESULTS-rust.md`
   and the test suite show, with the corpus, the command lines and "lower is better" stated next to
   every percentage.
5. **Format** stays the same in both builds: `docs/spec/TSAUR-FORMAT-SPEC-v1.0.md` registers
   Lepton as segment kind 2 of a stream recipe; a second implementation may use any Lepton decoder.

## 4. Reproducing the matrix

```bash
cd tsaur
cargo build --release                                            # full
cargo build --release --no-default-features --target-dir target-nolepton   # lite
./target/release/tsaur pack full.tsr ../benchmarks/corpus_real
./target-nolepton/release/tsaur pack lite.tsr ../benchmarks/corpus_real
./target-nolepton/release/tsaur info full.tsr --json | grep -A2 requires
./target-nolepton/release/tsaur unpack full.tsr out/    # exit 7: needs the full build
./target/release/tsaur unpack lite.tsr out/             # ok
```
