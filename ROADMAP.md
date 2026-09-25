# Roadmap (updated 2026-09-25)

## v1.0 gate (`docs/V1-CONTRACT.md`)
- [x] format v1 frozen, golden archives, cross-build compatibility check (`tools/compat_check.py`)
- [x] untrusted-data review of extraction, limits, links, overwrite; robustness campaign on the stable toolchain
- [x] distribution policy decided (lite recommended, full alongside with notices); packages built and checked from a clean directory (`tools/package.py`)
- [x] user guide executed end to end before a release (`tools/check_guide.py`)
- [x] test suite run on a second operating system: release gate executed on Linux x64 (`docs/review/RC1-VERIFICATION-REPORT-linux-x64.md`); CI matrix green on Linux, macOS and Windows with identical determinism hashes (run 36099317045); the release gate itself has not been run on macOS
- [ ] independent review by another evaluator (`docs/review/REVIEW-PACKAGE.md`)
- [ ] legal confirmation of the LGPL reading for the full binary (`docs/DISTRIBUTION-POLICY.md` §3.4)
- [ ] measurements on two real devices (`docs/design/TWO-DEVICE-BENCHMARK-PLAN.md`)

## Phase 0 — Research and decisions (done)
- [x] Research on 7 areas → `docs/research/`
- [x] Test corpus + reproducible local benchmark (7-Zip 26.03 and WinRAR 7.23 included) → `benchmarks/`
- [x] Python prototype validating the ideas (CDC + dedup + dictionary + solid + container-aware + canonical + encryption + pieces/RS) → `prototype/`
- [x] Specification draft v0.1 → `docs/spec/`
- [x] Name confirmed by the founder: **T-saur**, extension **`.tsr`**; logo concept in `docs/brand/`
- [x] Agent-first design document → `docs/DESIGN-AGENT-FIRST.md`
- [ ] Reservations: GitHub org `tsaur-format`, PyPI/npm/crates.io `tsaur`, domains tsaur.dev/.org/.io/.ai + t-saur.com (founder's accounts); formal trademark search (TMview)

## Phase 1 — Rust reference implementation, core (in progress)
- [x] workspace `tsaur/` with `tsaur-core` (format, FastCDC, BLAKE3, zstd + dictionaries, solid blocks, container recipes for ZIP/OPC, XChaCha20-Poly1305 + Argon2id, Ed25519 signatures, limits, pieces + Reed-Solomon parity) and `tsaur-cli` (`pack`, `unpack`, `list`, `verify`, `read`, `pieces`, `recover`, `keygen`, `--json`)
- [x] preflate-rs integration for ZIP/OPC members: deflate streams are inverted and recreated bit-exact whatever encoder produced them (verified on zlib-ng output from Python 3.14: 928 KB of DOCX XML carried with 2.3 KB of corrections)
- [x] preflate for PDF FlateDecode streams (precomp-style `stream … endstream` scanner, zlib header/Adler-32 kept, bit-exact rebuild verified): 10-file corpus → 70.5 % of original vs 7-Zip 73.6 % / WinRAR 74.2 %; corpus with versions → 50.3 % vs 54.9 % / 55.4 %
- [x] streaming packer (bounded memory: files are chunked as they are read, blocks are compressed in parallel batches and written immediately; only container files are held for preflate), memory-mapped reader, streaming extraction with verify-before-rename
- [x] block mode as default (64 KiB chunks for dedup/verification, 1 MiB blocks for compression): 71.4 % / 53.9 % on the two corpora vs 7-Zip 73.6 % / 55.0 % and WinRAR 74.2 % / 55.6 %, with random access per block; `--solid N` for maximum ratio, `--granular` for per-chunk access
- [x] robustness tests: 150 random byte-flip corruptions and 15 truncations must be rejected without panics; `cargo test --release` = 13 tests
- [x] agent operations in the CLI: `stat`, `grep` without extraction, `list --md` with token estimates, `unpack --entry <glob>`
- [x] xz/LZMA2 codec per block with block-sized dictionaries; `--codec best` (default) keeps the smaller of zstd -19 and xz 9e: corpus A 63.3 % / corpus B 48.2 % vs 7-Zip 73.6 % / 55.0 %; `--codec zstd --level 9` as the fast mode (400 MB in 1.9 s)
- [x] reference archives (`pack --ref base.tsr`): chunks already held by a reference archive are recorded as external and not stored; readers resolve them with `--ref`; unresolved chunks are reported, never silently skipped
- [x] PPMd (variant H, `ppmd-rust`) per block for text-like blocks, selected automatically by `--codec best`: text subset 22.7 % vs 7-Zip PPMd 23.5 % / LZMA2 25.4 % / WinRAR 26.8 %
- [x] Lepton lossless JPEG recompression (`lepton_jpeg`, Microsoft, Apache-2.0) for `.jpg` files and DCTDecode streams inside PDFs: JPEG set 43.2 % vs 7-Zip 50.4 % / WinRAR 50.5 %, bit-exact, deterministic (single-threaded)
- [x] x86 and ARM64 branch filters (BCJ) tried per block on machine code, stored in the blob record `f`; incompressible blocks skip the slow codecs: Windows DLL/exe corpus (13.5 MB) 32.5 % block / 30.8 % solid (7-Zip BCJ2 29.7 %, WinRAR 33.1 %); ARM64 .so corpus (22.5 MB) 17.8 % / 16.0 % (7-Zip 16.4 %, 15.9 % with filters off; WinRAR 18.5 %)
- [ ] section-aware executable filtering (parse PE/ELF/Mach-O headers, filter only code sections; BCJ2-class split streams) to close the remaining gap to 7-Zip on executables
- [x] delta codec (codec 5, zstd + chunk dictionary): new versions are coded against the earlier content held by the archive or a `--ref` archive; corpus B 48.1 % → 45.2 % in block mode (solid mode untouched), incremental archive by reference 6.8 % → 2.8 %
- [ ] LZMA-based delta (raw LZMA2 stream with a preset dictionary, matching the xz ratio that beats zstd on text and PDF content); section-aware executable filtering
- [ ] float-split codec for tensors, context-mixing tier
- [x] hybrid recipients X25519 + ML-KEM-768 (`ml-kem` 0.3 + `x25519-dalek` 3, HKDF-SHA-256 combiner): `keygen --recipient`, `pack --to`, `unpack --identity`; recipients block = list of stanzas (argon2id and/or x25519mlkem768)
- [x] streaming pieces/parity: sidecar creation, verification and repair one stripe at a time (≤ 64 MiB per stripe), validated sidecar tables, exact-length restoration, partial repair reporting
- [x] `tsaur info` (sections, codecs, filters, chunk/blob statistics, references) and credential-free `inspect` (framing, stanza types, signature key) for locked archives
- [x] LICENSE-APACHE / LICENSE-MIT, SECURITY.md, CONTRIBUTING.md, CHANGELOG.md
- [ ] composite ML-DSA-65 signatures, FIDO2 stanza
- [x] fuzz-style robustness tests in `cargo test` (archive bit flips/truncations, sidecar corruption, adversarial filter inputs); cross-OS determinism job in CI; `--jobs` determinism test
- [ ] cargo-fuzz targets on nightly (reader, container parsers, canonical converters); golden test vectors (CC0)
- [x] real-world files validated (python-docx template, matplotlib PDFs, scikit-learn/matplotlib JPEGs, a 100 KB paper PDF): 13 containers exploded, 0 fallbacks, 13/13 bit-exact; 78.3 % vs 7-Zip 92.1 % / WinRAR 92.3 %; `benchmarks/build_extra_corpora.py` rebuilds the optional corpora (real, x86-64 binaries, ARM64)
- [ ] remaining checks from `docs/DESIGN-AGENT-FIRST.md` §5 (Romanian corpus, large mixed corpus, PQ crate maturity review, iroh 1.0, exact token counting, dependency license audit, Windows ADS/long paths)

## Phase 2 — Semantic layer + agents
- [x] first canonical views in Rust: `read --view canonical` renders DOCX → Markdown (headings, lists, tables, from the document XML) and PDF → text (`pdf-extract`), with a provenance header and an explicit non-bit-exact label
- [x] stored derivatives (hybrid fidelity, `pack --canonical`): `.tsaur/views/<path>.md|.txt` entries with `derived {from, view, generator, tokens_est}`, served by `read --view canonical` and the MCP `tsaur_read` tool, skipped by default extraction; corpus A 62.2 % → 65.4 % for 4 views
- [ ] richer converters (pptx/xlsx, PDF layout/tables via local Docling/MarkItDown backends, no paid OCR); exact token counts (tokenizer) instead of the chars/3.5 estimate
- int8/binary embeddings + rebuildable HNSW index; summaries with declared model hash
- [x] MCP server `tsaur mcp` (stdio JSON-RPC; tools `tsaur_info`, `tsaur_list`, `tsaur_stat`, `tsaur_read` with byte/line ranges and canonical views, `tsaur_grep`, `tsaur_verify`, `tsaur_unpack`; resources `tsaur://<archive>/<entry>`; `--root` sandbox; reader cache; end-to-end test)
- [x] `diff` between archives (entries added/removed/changed, chunks shared with the older archive → what `--ref` would save) and citation URIs `tsaur://<merkle-root>/<path>#L..` in `stat`, `list --json` and MCP reads
- [ ] `search` tool (embeddings), A2A bindings
- compact manifest projection (`TSAUR.md`) with pre-computed token counts; Unicode sanitisation; trust levels

## Phase 3 — Distribution + recovery
- [x] offline volume sets (`tsaur volumes split|inspect|join|repair`): N data + M parity volumes over directories/drives, self-describing volumes, any N of N + M rebuild the archive bit-exact, byte-identical repair, placement warning; design note `docs/design/VOLUME-SETS.md`
- [x] direct exchange of missing pieces between two T-saur instances with manually supplied addresses (`tsaur volumes serve` / `fetch`): verified descriptor and pieces, fewest volumes needed, peer failover, resume; trust contract `docs/design/VOLUME-TRUST.md`
- [x] transfer stabilisation: resume verified from disk, explicit resource limits, loopback-only default with `--expose-lan`, network-derived names never choose local paths
- [x] encrypted, peer-authenticated transport with locally pinned identities: TLS 1.3 (`rustls`, ring) with self-signed certificates pinned by SHA-256 fingerprint on both sides (`volumes keygen`, `serve --tls-identity --allow`, `fetch --peer-id`); no CA, account or server
- [x] per-address request rate, per-connection time budget, explicit `--allow`/`--allow-anyone` for any network exposure
- [ ] measurements on two real devices (`docs/design/TWO-DEVICE-BENCHMARK-PLAN.md`, tooling in `benchmarks/two_device_bench.py`; not yet run on two devices)
- [ ] independent review of the exchange path by another evaluator (`docs/review/REVIEW-PACKAGE.md`; the producer's own checks are not that review)
- [ ] local-network discovery, resumable multi-peer scheduling, NAT traversal / relays (opt-in, never required); public DHT only as an optional later addition
- `.tsr.pieces` sidecar + Bao verified streaming (BLAKE3); RaptorQ (RFC 6330) as an optional fountain mode
- iroh transport for agent-to-agent transfer; hybrid BitTorrent v1/v2 export; CAR/IPFS export; OCI artifact mapping
- optional anchoring of the manifest root in OpenTimestamps / Sigstore Rekor (free)
- external reference sets ("context packs", public corpora)

## Phase 4 — Advanced codecs (experimental)
- context mixing for small critical texts; model-predictive codec with a pinned small open model (deterministic integer inference)
- lossless JPEG recompression (JPEG XL / lepton-class); delta between versions

## Phase 5 — Ecosystem
- Python/Node bindings, spec 1.0 under CC-BY-4.0, codec registry, conformance suite
- integrations with agent platforms (Skills, Files APIs), repomix/gitingest-style views
