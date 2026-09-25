# What an AI agent needs from an archiver — and how I would build it

**Author:** Claude (Fable 5.1), at the maintainer's request · **Date:** 22 September 2026 · **Status:** design document for T-saur (formerly working name AIX), written in the first person, from the perspective of an agent that works with files every day.

---

## 0. How I actually work with files (the premise of the entire design)

To design an archiver "for agents" I have to start from what I actually do, not from what a human does with WinRAR:

1. **I don't "open" files, I make calls.** I read with `Read(path, offset, limit)`, search with `Grep(pattern)`, list with `Glob`. Every call has a fixed cost (latency + tokens) and a ceiling: a single `Read` returns me at most ~25,000 tokens, after which I have to paginate. So I want **partial, stably addressable reads** (lines, bytes, pages, sections), not "extract everything and see".
2. **I don't see bytes, I see text.** A PDF reaches me either as text extracted page by page or as images; a DOCX I can read only after conversion. For me, **the canonical representation (Markdown/JSON) is the document**, and the original bytes are evidence that I verify by hash when it matters.
3. **Tokens are my budget.** A listing of 10,000 files with 64-hex hashes costs me more than the content I am looking for. I want manifests that are **tabular, short, with abbreviated hashes and pre-computed token counts**, so I can plan what I load.
4. **I verify, I don't trust.** After I edit or extract, I recompute hashes through the shell. I want the archive to give me **per-file and per-piece verification**, with a structured report, without extracting everything.
5. **I am the target of content-borne attacks.** Any text in a file may try to give me instructions (indirect prompt injection). I want the format to label **every entry with a trust level** and to strictly separate *data* from *signed instructions*.
6. **I need determinism.** If the same input produces different archives, I cannot cache, cannot compare, cannot prove. I want **bit-for-bit reproducible archives** and identical extraction results on any machine.
7. **I work in a sandbox, with limited network and no privileges.** Any dependency on a paid service, a GPU or a 14 GB model makes the format unusable for me in 90 % of situations. I want the **default tier to be CPU-only, offline, free**.
8. **I cite sources.** When I answer, I want to be able to point exactly to "document X, chunk 12, lines 120–180" with a stable (content-addressed) identifier, not a file path that changes.
9. **I do small, repeated, sometimes interrupted operations.** I want idempotent, resumable commands with JSON output and stable error codes; never interactive prompts.
10. **I have no memory across sessions other than through files.** An archive that describes its own contents (manifest + `TSAUR.md` view) is, for me, portable memory.

---

## 1. The operations I need (the minimal API, from the agent's perspective)

| Operation | What it returns to me | Why it matters to me |
|---|---|---|
| `list [--filter glob/type/size/trust] [--page]` | compact table (path, type, size, tokens, trust, short hash), paginated | ≤ 2,000 tokens for 100 entries; I decide what to load |
| `stat <entry>` | full hash, size, available derivatives (canonical/structure/raw), tokens per view, provenance, trust | planning + citation |
| `read <entry> --view canonical --lines 120-180` / `--bytes a-b` / `--page 3` / `--section "## Intro"` | the requested fragment + stable anchor `tsaur://<id>/<path>#L120-180` | partial, addressable reading, without extraction |
| `grep <regex> [--in canonical] [--entries glob]` | matches with path + line + context | I search inside the archive without unpacking it |
| `search "<question>" --top 5` | relevant chunks (embeddings) with location and score | semantic access to large collections |
| `verify [<entry>] [--pieces] [--deep]` | JSON report: ok/corrupt per file, per piece, signatures, anchorings | verifiable trust, not assumed trust |
| `extract [<entries>] --to <dir> [--dry-run] [--view raw\|canonical]` | files on disk with sanitized paths | selective, safe, predictable extraction |
| `cite <entry> [--range]` | stable identifier + hash + provenance, ready to put into an answer | traceability |
| `diff <A> <B>` | which chunks/entries changed between two archives | CDC makes the diff almost free |
| `derive <entry> --view canonical` | generates the missing derivative, with the tool and version recorded | on-demand conversion, with provenance |
| `pack <files> --profile p2p\|archive\|agent [--canonical] [--recipients …]` | deterministic archive + JSON report | reproducible creation |
| `fetch <hash\|entry>` | fetches the missing chunks from peers/URLs in `hints` | partial archives, "compression by reference" |

Cross-cutting rules: all commands have `--json`, stable exit codes (0 ok, 2 corrupt, 3 refused by policy, 4 resources exceeded, 5 missing chunks), zero interactivity, zero writes outside the target directory, default resource limits (anti-bomb).

---

## 2. My requirements, ordered by importance

**A. Reading and inspection**
- R1 Model-readable manifest: canonical dCBOR on disk + deterministic `TSAUR.md` view (tabular, hashes of 8–12 hex digits, pre-computed o200k/claude token counts). → spec §6.
- R2 Partial reading with stable addressing by lines/bytes/pages/sections; solid blocks of at most 1–4 MiB or granular chunks make any fragment accessible with a single small decompression. → spec §4.2.
- R3 Inline canonical derivatives (Markdown/JSON/text) with converter provenance (`by.tool`, `by.version`, `deterministic: false` where applicable) and declared fidelity. → spec §7, §11.
- R4 Search index (grep on the canonical view; optional int8/binary embeddings), rebuildable, never executable. → spec §7.

**B. Integrity and verification**
- R5 BLAKE3 hash per chunk, per file, per archive; signed Merkle root; verification without decryption (verify-cap over ciphertext). → spec §8, §10.
- R6 Verification per 16 KiB piece (compatible with Bao/iroh and BEP 52) so I can validate what I received before writing to disk. → spec §10.
- R7 Recovery record (Reed-Solomon, 5–20 %) with hashed symbols: I repair locally, without asking for everything again. → spec §10.

**C. Security**
- R8 Data ≠ instructions: nothing in the content is a command; instructions are typed entries, signed by keys from **my** allowlist, with `trust` 0–3 enforced by the reader. → spec §6.
- R9 Per-chunk AEAD (XChaCha20-Poly1305), envelope keys, Argon2id, hybrid X25519 + ML-KEM-768, Ed25519 + ML-DSA-65; encrypted manifest. → spec §8.
- R10 Unicode sanitization (NFC, removal of U+E0000–E007F, zero-width, bidi) in every view I read; original kept intact. → spec §6.
- R11 Hard limits: total, per entry, ratio, nesting depth; paths as labels, never as destinations. → spec §9.

**D. Determinism and reproducibility**
- R12 Same files + same parameters ⇒ same bytes ⇒ same root, on Windows/Linux/macOS, x86/ARM (dCBOR, canonical ordering, no implicit timestamps). → spec §0 G6.
- R13 The default codecs are deterministic on CPU (zstd, xz, RS, preflate); any neural tier is opt-in, with the model pinned by hash and mandatory round-trip verification before the archive is written. → spec §4.2.

**E. Compression**
- R14 Default tier 1: 64 KiB CDC + dedup + zstd with per-content-type dictionaries, hundreds of MB/s, no GPU. (From the compression-theory research, the maintainer's working material.)
- R15 Container-aware: DOCX/XLSX/PPTX/EPUB/ZIP exploded, PDF with streams inverted through preflate, JPEG through Lepton/JPEG XL — all with bit-exact verification and fallback to store. → spec §5.2.
- R16 External references and shared dictionaries addressed by hash (the RFC 9842/CRAM model): I send only what the receiver does not have. → spec §4.3, §5.3.
- R17 Solid blocks for ratio + granular for P2P, chosen per content type, not globally. → spec §4.2.

**F. Distribution**
- R18 Fixed-size pieces with a Merkle root (`.pieces` sidecar), parity in `.par`, peer hints; CAR/torrent v2/OCI export; iroh transport. → spec §10.
- R19 Partial archives as the normal state: I can start reading the manifest and the canonical derivatives before all the blobs exist. → spec §10.

**G. Tools and integration**
- R20 CLI with `--json`, Rust library + Python/Node bindings, MCP server exposing `tsaur://` resources and a `read_range` tool. → ROADMAP.md phase 2.
- R21 Structured error messages (code, cause, what is missing, how to fix it); no interactive prompts.
- R22 Resumable operations (pack/fetch/verify) with on-disk state; idempotent.

**H. Cost and openness**
- R23 Free and open source, with no paid services in the default path: OpenTimestamps (free) for anchoring, public Rekor (free) optional, public or self-hosted iroh relays, no commercial OCR (Docling/MarkItDown locally), no models with restrictive licenses.
- R24 Licenses: Apache-2.0 (code), "Apache-2.0 OR MIT" for crates, CC-BY-4.0 (spec; an OWFa 1.0 patent commitment was considered and not adopted), CC0 (test vectors). No AGPL components in the production path (PyMuPDF from the prototype is AGPL — to be replaced with pdfium-render/lopdf in Rust).

---

## 3. What I would take from each archiver (and what I would leave behind)

| Source | I take | I leave |
|---|---|---|
| ZIP | index at the end + random access; ubiquity as an **export format** | two sources of truth (LFH vs CD), ZipCrypto, lack of a solid mode |
| 7-Zip / 7z | solid blocks, method chain (filters + codec), PPMd for text, encrypted header | AES-CBC without MAC, LZMA-only |
| RAR 5 / WinRAR 7 | recovery record as a core feature, solid, long-range matching, volumes | proprietary compressor, no AEAD, 64 GB dictionary in RAM |
| zstd | trained dictionaries, `--patch-from`, `--long`, seekable frames, decompression speed | — |
| brotli / RFC 9841-9842 | shared dictionaries addressed by hash (the "reference" model) | the English-only static dictionary |
| xz / LZMA2 | maximum LZ ratio for the "archive" profile | the supply-chain lesson: reproducible build, no blobs |
| ZPAQ | CDC dedup + append-only journaling + "the archive describes itself" | SHA-1, CTR without MAC, executable bytecode |
| Precomp / preflate-rs / Lepton | bit-exact inversion of deflate/JPEG with fallback | — |
| Hugging Face Xet | 64 KiB Gear CDC, 64 MB xorbs, 3-level dedup, protected hashes | dependence on a central hub |
| BitTorrent v2 / iroh | verification at 16 KiB, per-file Merkle, verified streaming (Bao), tickets | pure v2 swarm (weak adoption) |
| age / Tahoe-LAFS | STREAM AEAD over chunks, per-recipient stanzas, verify/read/write capabilities | — |
| OCI / CAR | manifest + content-addressed layers, referrers for SBOM/signatures | lack of derivatives and trust levels |
| Agent Skills / llms.txt / repomix | progressive loading (metadata → instructions), text view for the LLM | flat text without integrity or compression |
| Docling / MarkItDown | canonical representations with provenance (DoclingDocument, DocTags) | conversion as a replacement for the original |

---

## 4. How I would build it (implementation architecture)

### 4.1 Stack
- **Rust** for everything that parses bytes (memory-safe, fuzzable, single binary): `blake3`, `zstd`/`zstd-safe`, `fastcdc`, `preflate-rs` (Microsoft, Apache-2.0), `lepton_jpeg` (Rust, Apache-2.0), `chacha20poly1305` + `argon2` + `hkdf` (RustCrypto), `x25519-dalek`, `ml-kem` and `ml-dsa` (RustCrypto, audit status to be verified), `ed25519-dalek`, `dcbor`/`ciborium` + `coset` (COSE), `reed-solomon-simd` or `leopard` for parity, `raptorq` (later), `iroh-blobs` (transport), `zip` (OPC containers), `lopdf`/`pdfium-render` (PDF), `unicode-normalization` + a table of invisible characters.
- **Bindings**: PyO3/maturin (Python), napi-rs (Node); **MCP server** in Rust (stdio + HTTP) exposing `tsaur://` resources and `list/read_range/grep/search/verify` tools.
- **Canonical converters** as external plugins with recorded provenance: Docling / MarkItDown (local, free); never commercial OCR in the default path.

### 4.2 Crates and responsibilities
```
tsaur-format     binary format, sections, dCBOR, chunk table, limits (no network I/O)
tsaur-chunk      FastCDC/Gear (public table / key-derived), Bao outboard, Merkle
tsaur-codec      store | zstd(+dict) | xz | delta | float-split; codec registry, per-type selection
tsaur-container  zip/OPC, pdf (preflate), jpeg (lepton), tar/gzip — recipe + bit-exact verification + fallback
tsaur-crypto     archive key, HKDF, AEAD STREAM, stanzas (argon2id, x25519mlkem768, fido2, shamir), composite signatures
tsaur-manifest   entries, derivatives, trust, policies, views (TSAUR.md, JSON/TOON), Unicode sanitization
tsaur-dist       pieces, RS parity, sidecars, CAR / torrent v2 / OCI export, iroh client, OTS/Rekor anchoring
tsaur-cli        commands + --json + exit codes
tsaur-mcp        MCP server (resources + tools)
```

### 4.3 The pack flow (deterministic)
1. Canonical enumeration (NFC paths, bytewise order, no implicit timestamps) → 2. container detection + explosion with a recipe → 3. CDC over the concatenated logical stream → 4. hash-based dedup (keyed in encrypted mode) → 5. grouping into blobs (solid per type / granular) → 6. per-blob codec selection (zstd+dict / xz / store / delta) with round-trip verification → 7. canonical derivatives + tokens + (optional) embeddings → 8. dCBOR manifest + `TSAUR.md` view → 9. encryption (if requested) → 10. signing → 11. sidecars (pieces + parity) → 12. JSON report.

### 4.4 Performance targets (to be measured in CI, on a declared machine)
- `list` under 50 ms for 10,000 entries; `read --lines` under 20 ms + the decompression of a single block;
- tier 1 pack ≥ 200 MB/s on 8 cores (CDC ~1 GB/s, zstd -3..-9), "archive" tier at the speed of xz -9;
- verify at BLAKE3 speed (≥ 1 GB/s/core); extract at zstd speed (≥ 1 GB/s);
- metadata overhead < 1 % with 64 KiB chunks; compressed manifest.

### 4.5 Testing and security
- round-trip corpora (Silesia, Canterbury, enwik8, office corpus with versions, **Romanian corpus**), CC0 "golden" vectors, cross-OS/arch determinism tests in CI;
- continuous fuzzing (cargo-fuzz) on the parser, codecs, containers, dCBOR; differential tests against the Python prototype;
- threat model maintained in the repo (from the security research, the maintainer's working material), external cryptographic review before 1.0, reproducible build + SLSA provenance, `cargo deny`/`cargo audit`, ≥ 2 maintainers with release rights.

---

## 5. What still needs to be verified (review of the research)

I re-read the seven research documents (the maintainer's working material, not published); they are solid, but they have gaps that I would close before writing production code:

1. **Romanian text.** Our entire benchmark is in English. The CM/LLM figures (ts_zip, cmix) are on enwik; EACL 2026 shows that small LLMs lose a lot on non-English languages. We need a Romanian corpus (real documents, diacritics) and zstd/xz/PPMd/trained-dictionary measurements on Romanian. *Status 2026-09-23: first measurement on the 10 Romanian research drafts (a private corpus, not published; 437 KB of Markdown with diacritics): T-saur default 26.6 % (PPMd chosen automatically; zstd alone 32.2 %, xz alone 31.3 %) vs 7-Zip LZMA2 31.0 %, 7-Zip PPMd o32 28.6 %, WinRAR 32.6 %; `grep` with diacritics works. Real Romanian documents (DOCX/PDF) and the LLM tier are still to be measured.*
2. **Real DOCX/PDF.** Our DOCX files are generated by python-docx (zlib deflate, trivially reconstructible); our PDFs by PyMuPDF. preflate-rs must be tested on files from Word/LibreOffice/Adobe/scanners and the fallback rate measured (published preflate results report 20–30 % of streams with differences). *Status 2026-09-23: first real-world set (python-docx template built by Word, matplotlib PDFs, a 100 KB paper PDF, sklearn/matplotlib JPEGs; `benchmarks/build_extra_corpora.py`): 13 containers exploded, 0 fallbacks, 13/13 bit-exact, 78.3 % vs 7-Zip 92.1 % / WinRAR 92.3 % (`benchmarks/RESULTS-rust.md`). Word/LibreOffice/Adobe/scanner originals still to be collected.*
3. **Real JPEG.** Lepton/JPEG XL on real photographs (progressive, arithmetic) — reconstruction rate and actual gain. *Status: baseline photographs (sklearn china/flower, grace_hopper, a magazine cover) recode with Lepton at ~22 % gain, 0 failures; progressive/arithmetic JPEGs fall back to raw storage by design (Lepton rejects them) and still need a measured sample.*
4. **7-Zip and WinRAR** — now installed; the benchmark is being redone with them (LZMA2, PPMd, RAR5 solid, RAR with recovery record) for a direct comparison, not through the xz proxy. *Done: `benchmarks/RESULTS-rust.md` (7-Zip 26.03 LZMA2 -mx9 and default filters, WinRAR 7.23 RAR5 -m5 -mcx) on every corpus.*
5. **Large corpus.** The 1.5 MB scenario is illustrative; dedup and dictionaries are judged on tens of GB (backups, repos, document collections with versions).
6. **Cross-platform determinism** of archives: the same bytes on Windows/Linux/macOS and x86/ARM (zstd with the same parameters is deterministic; it must be verified that we do not depend on directory order or on the system zlib). *Status: inputs are sorted, all codecs are statically linked Rust/C code (zstd, liblzma, ppmd-rust, preflate-rs, Lepton single-threaded); `.github/workflows/ci.yml` compares the archive hash across the three OSes and will run on the public repository.*
7. **Maturity of the PQ libraries in Rust** (`ml-kem`, `ml-dsa` from RustCrypto): audit status, compatibility with the FIPS vectors, stable API; alternative: liboqs via FFI.
8. **iroh-blobs 1.0**: recent breaking API change (0.103), the cost of public relays, behavior without a relay (LAN/agent-to-agent).
9. **RaptorQ vs RS** on real loss patterns (missing pieces vs corrupted bytes) and the hash overhead for the repair symbols.
10. **Token counting**: tiktoken/o200k and the Claude tokenizer on real `TSAUR.md` views, so that the "≤ 2,000 tokens/100 entries" target is measured, not assumed.
11. **Name and trademark**: formal TMview/EUIPO/USPTO check for the chosen name (automated searches are blocked), plus a domain search with a real registrar.
12. **Contradictory figures flagged by agents** (DeepMind abstract vs table; LMCompress "4×" vs figure; ZipNN "month" vs "year"; libtorrent versions) — both values are cited, the favorable ones are not picked.
13. **`[unverified]` markers** (~70 in total) — resolve those that influence decisions: RS in RAR5, the blake3 multihash code, Sia 10-of-30, the Swarm erasure parameters, the C2PA 2.4 date, OpenAI limits.
14. **Licenses of the prototype's dependencies**: PyMuPDF is AGPL-3.0 — it cannot go into the final product; python-docx MIT ok; pypdf BSD ok.
15. **Patents**: FastCDC (academic paper; Xet uses Gear freely), Lepton (Apache-2.0 with grant), preflate (Apache-2.0); a quick audit to be done before 1.0.
16. **Windows specifics**: ADS (`:`), long paths (`\\?\`), reserved names (CON, NUL), case-insensitivity — the extractor must be tested explicitly (CVE-2025-8088 came from exactly here). *Status: `paths.rs` rejects `:`/ADS, reserved names, trailing dots/spaces, absolute and `..` paths (test `rejects_unsafe_paths_on_extract`); paths above the classic MAX_PATH limit round-trip (`long_paths_roundtrip`); entry sets that differ only by letter case are refused on Windows/macOS instead of overwriting (`case_colliding_entries_are_refused_on_case_insensitive_filesystems`).*

---

## 6. What comes next (4 weeks, with acceptance criteria)

| Milestone | Deliverable | Acceptance |
|---|---|---|
| M0 (week 1) | name confirmed (T-saur), reservations pending; benchmark redone with 7-Zip/WinRAR + Romanian corpus + real DOCX/PDF; spec §13 decisions closed | `benchmarks/RESULTS.md` v2; spec v0.2 with no blocking "open questions" |
| M1 (week 2) | `tsaur-format` + `tsaur-chunk` + `tsaur-codec` in Rust: bit-exact pack/unpack/list/verify, dCBOR, limits | round-trip on all corpora; identical archives on 3 OSes in CI; 24 h of fuzzing without a crash |
| M2 (week 3) | `tsaur-container` (zip/OPC + preflate) + `tsaur-crypto` (AEAD, Argon2id, X25519+ML-KEM, signatures) | fallback rate reported; crypto test vectors; internal review of the threat model |
| M3 (week 4) | `tsaur-manifest` (canonical derivatives, trust, `TSAUR.md`, tokens) + `tsaur-cli --json` + minimal `tsaur-mcp` (`list`, `read_range`, `verify`) | an MCP agent reads an archive of 1,000 documents without extracting it; token budget measured |

After M3: `tsaur-dist` (pieces, parity, CAR/torrent/OCI, iroh), embeddings + search, "archive" tier (PPMd/CM), experimental LLM tier.
