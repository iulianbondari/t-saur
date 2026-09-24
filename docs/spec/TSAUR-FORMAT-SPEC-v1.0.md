# T-saur Archive Format (.tsr) — Specification v1.0

> Status: **v1.0, format frozen** (2026-09-24; drafted as v0.1 on 2026-09-22). Name: **T-saur**, file extension **`.tsr`**; volume sets **`.tsrv`**. Former working name "AIX".
> Language: English. License of this document: CC-BY-4.0.
>
> **What is frozen and what is not.** §3 (file layout), §4 (chunking and identity), §5 (codecs 0–5 and filters 1–2, blob records), §6 (entries, containers, references), §8 (encryption, recipients, signatures), §9 (limits), §10 (pieces, parity, volume sets) and §12 (versioning) describe format version 1 as the reference implementation writes and reads it; the golden archives in `tsaur/crates/tsaur-core/tests/golden/` are the executable form of this freeze. Sections that describe reserved codecs, the semantic layer beyond stored canonical views, trust levels, external reference sets, anchoring and the P2P layer beyond volume sets and the `TSXP/1` exchange are **design intent**, marked "reserved" or "future", and are not promised by v1.0 (`docs/V1-CONTRACT.md` lists what is). Implementation-status notes inside the text record when a feature landed; "v0.1" in those notes is the name of the first implementation, which v1.0 keeps unchanged at the byte level.
>
> **Abstract:** this document describes the proposed archive format: a content-addressed container (chunks identified by hash, Merkle tree), layered compression (dedup → dictionaries/references → strong codecs → optional language model), two fidelity modes (bit-exact and canonical/semantic for agents), per-chunk envelope encryption, signatures, trust levels for content, and a P2P distribution layer (verifiable pieces, erasure coding, optional anchoring in transparency logs/DLT). Every decision carries a short rationale. Prototype numbers are in `benchmarks/RESULTS.md`.

---

## 0. Design goals and non-goals

| # | Goal | Why |
|---|------|-----|
| G1 | **Agent-native.** The archive is a self-describing *knowledge container*: an AI agent can read the manifest, decide what it needs, fetch only those chunks, and get a canonical (Markdown/JSON) view of documents without ever touching PDF/DOCX parsers. | Agents are the intended consumer; bytes are secondary to information. |
| G2 | **Information-based compression.** Store *new information* only: deduplicate content-defined chunks, use shared dictionaries and external references ("the receiver already has this"), then apply strong codecs; optionally use a pinned language model as a predictor. | This is the only physically valid route to "much smaller than RAR/ZIP": exploit redundancy and prior knowledge, not magic. |
| G3 | **P2P-native.** The archive is a Merkle DAG of chunks; any subset of pieces is independently verifiable; missing pieces can be fetched from any peer or reconstructed from erasure-coded parity. | BitTorrent v2 / IPFS / iroh lessons; recovery records like RAR/PAR2 but standardised. |
| G4 | **Secure by construction.** Authenticated encryption per chunk, envelope keys, multi-recipient (hybrid post-quantum), signed manifests, encrypted metadata, resource limits against decompression bombs, no path traversal by design. | Every legacy format has at least one of: unauthenticated encryption (7z), weak KDF (WinZip AES), plaintext metadata, Zip Slip. |
| G5 | **Honest fidelity.** Two explicit modes: `bit-exact` (lossless bytes, verified by hash) and `canonical` (semantic-lossless derivative for agents). Never blur the two. | Lossy "AI compression" presented as lossless would be a trust failure. |
| G6 | **Deterministic and reproducible.** Same inputs + same parameters ⇒ byte-identical archive ⇒ same root hash. | Enables cross-archive dedup, caching, and audit. |
| G7 | **Open.** Spec under CC-BY-4.0, reference implementation Apache-2.0 (patent grant), no proprietary codec. | RAR's proprietary compressor limited its ecosystem; ZIP and zstd won by openness. |

Non-goals (v0.1): GUI, Windows shell integration, self-extracting executables, in-place update of archives (append-only journaling is v0.2), compression of already-compressed media beyond container awareness.

---

## 1. Terminology

- **Chunk** — a content-defined slice of a logical byte stream, identified by `BLAKE3-256(chunk_bytes)`. Immutable, deduplicated.
- **Stream** — the logical bytes of one object (a file, a container member, a canonical derivative) = ordered list of chunk references.
- **Blob** — a compressed (and optionally encrypted) unit stored in the archive body: either one chunk (*granular mode*) or a *solid block* of several consecutive unique chunks (*solid mode*).
- **Piece** — a fixed-size slice of the *archive file* used for transport/verification/erasure coding (like a BitTorrent piece). Pieces are orthogonal to chunks.
- **Entry** — a manifest record describing one input object (path, size, hash, mode, streams, derivatives, trust, provenance).
- **Reference set** — a named, content-addressed set of chunks or a trained dictionary that the receiver is expected to have (or can fetch) and that the archive does not embed.
- **Manifest** — canonical CBOR document describing everything; hashed and signed.
- **Root** — `BLAKE3` Merkle root over the chunk table (content identity of the archive, independent of compression/encryption/layout).

---

## 2. Layered model

```
L4  Distribution : pieces, piece hashes, Merkle root of pieces, erasure parity (RaptorQ/RS), peer hints, anchors (Rekor/OpenTimestamps)
L3  Semantic     : canonical derivatives (Markdown/JSON), summaries, embeddings index, knowledge-graph triples, tokens-budget hints
L2  Manifest     : entries, provenance (C2PA/in-toto-style), trust levels, policies, signatures, AI-BOM (CycloneDX ML-BOM)
L1  Objects      : streams (chunk lists), container recipes (zip/pdf/tar), delta references, external references
L0  Bytes        : chunks (CDC), blobs (codec + AEAD), dictionaries / reference sets
```

Each layer can be consumed independently: a P2P node needs only L4 + blobs; an agent deciding relevance needs only L2 + L3; a byte-exact restore needs L0 + L1.

---

## 3. File layout

```
+--------------------------------------------------------------+
| Header (fixed 32 bytes)                                      |
|   magic "TSR\x1A" (4) | version u16 | flags u16 | header_len u32   |
|   reserved (20 bytes, zero)                                        |
+--------------------------------------------------------------------+
| Section 1: recipients (cleartext CBOR: KDF params, wrapped key)    |  [if encrypted]
| Section 2: dictionary / reference-set stubs               [sealed] |
| Section 3: blobs (body) ... (append-only while packing)            |
| Section 4: chunk index (CBOR, zstd, sealed)                        |
| Section 5: manifest (canonical CBOR, zstd, sealed)                 |
| Section 7: semantic index (optional, zstd, sealed)                 |
| Section 6: signatures (CBOR: alg, public key, signature)           |
| Section table: CBOR array of {t, off, len, h = BLAKE3(section)}    |
| Trailer (24 bytes): table_off u64 | table_len u64 | crc32(table) u32 | "RST\x1A" |
+--------------------------------------------------------------------+
```

Rules:
1. Writers stream: header → sections → section table → trailer. Readers open from the trailer (like ZIP's end-of-central-directory): the table must end exactly where the trailer starts, its CRC must match, every section must lie inside `[header_len, table_off)` in increasing, non-overlapping order, and every section hash must verify **before** anything is decoded. There is exactly one source of truth for every value; redundant fields that disagree make the archive corrupt.
2. Every section has its own BLAKE3 hash in the section table; the signature covers the header and every non-signature table row (type, offset, length, hash), so the whole archive is authenticated.
3. All multi-byte integers little-endian in fixed headers; all structured data is **canonical CBOR** (RFC 8949 §4.2, deterministic encoding; struct field order fixed) so archives are reproducible.
4. Optional **sidecars** (same base name): `.tsr.pieces` (piece layer, like a `.torrent`), `.tsr.par` (erasure parity), `.tsr.sig` (detached signatures/attestations), `.tsr.idx` (semantic index if kept out of the main file).
5. Sealed sections and blobs use XChaCha20-Poly1305 with a nonce derived from the archive's nonce key, a label (blob / index / manifest / dictionary) and the blob index; the AAD binds the blob header (codec, uncompressed length, first chunk, chunk count) — see §8. Implementation status: `tsaur-core` v0.1 implements sections 1–6 exactly as drawn; section 7 is reserved.

---

## 4. Layer L0 — chunks, blobs, dictionaries

### 4.1 Chunking
- Algorithm: **FastCDC** (gear hash, normalized chunking level 2), parameters recorded in the manifest. Gear table and mask are **public and fixed** in `shareable` mode (cross-archive dedup, Xet-compatible: 16-bit mask ⇒ ~64 KiB); in `private` mode the gear table is derived from the archive key (defeats chunk-size fingerprinting attacks on encrypted archives, at the cost of cross-archive dedup).
- Default profile `p2p`: min 16 KiB / avg 64 KiB / max 256 KiB (Hugging Face Xet uses 8/64/128 KiB; casync 64 KiB). Max ≤ 256 KiB keeps every chunk under the IPFS Bitswap 2 MiB block limit. Profile `archive`: avg 256 KiB (less metadata). Profile `fine` (small text corpora): avg 8 KiB.
- Small files are chunked on the **concatenated logical stream** (casync model) so that a 3 KiB `.md` still dedups against its neighbours.
- Chunk ID = `BLAKE3-256(bytes)`; Bao outboard trees are computed at 16 KiB chunk groups (iroh-blobs compatible). Optional `sha2-256` per file (16 KiB-leaf Merkle, BEP 52 layout) recorded for BitTorrent v2 / OCI / IPFS interoperability when the `interop` flag is set.
- Dedup: identical chunk IDs are stored once per archive and may be *external* (see 5.3).

### 4.2 Blobs and codecs
| codec id | name | use |
|---|---|---|
| 0 | store | incompressible data (media, encrypted payloads) |
| 1 | zstd (level 3..22) | default; with or without dictionary; `--long` window for solid blocks |
| 2 | zstd + dictionary | granular mode: chunks compressed against a trained dictionary (reference set) |
| 3 | xz / LZMA2 | max-ratio option for text where speed is irrelevant |
| 4 | PPMd (variant H, 7z range coder; `p = [order, mem_size]`) | text blocks; beats LZMA2 by 2–4 points on prose |
| 5 | zstd + chunk dictionary ("delta"): `d` lists the chunks (internal or external) whose concatenation is the raw-content dictionary | new versions of documents already held by this archive or by a reference archive (the `--patch-from` model) |
| 6 | brotli | reserved; web-friendly |
| 7 | context-mixing (cmix/paq-class) | reserved; small critical text; slow (KB/s), deterministic |
| 8 | model-predictive (arithmetic coding with a pinned LM) | reserved tier; requires `model_id` = hash of weights + tokenizer + runtime spec; deterministic int8 inference, logit quantisation, mismatch-tolerant coding (PMATIC-class) as safety net; text only, languages the model covers (small English LMs lose on non-English text) |
| 9 | float-split (ZipNN-style exponent/mantissa separation + entropy coding) | reserved; BF16/FP16/FP32 tensors inside safetensors/GGUF/.pt; ~17–33 % lossless at GB/s |

A blob records `{off, clen, ulen, codec, n: [chunk indices…], p: [codec parameters…], f: filter, d: [dictionary chunk indices…]}` (the chunk list is explicit so that external/referenced chunks can interleave with stored ones in the chunk table; `p`, `f` and `d` are omitted when empty/zero); the AEAD AAD binds `codec, ulen, n[0], len(n)`. The reader must refuse blobs whose `ulen` exceeds the manifest-declared limits (anti-bomb, see §9), whose chunk sizes do not add up to `ulen`, that share a chunk with another blob, or that use an unregistered codec or filter id. Implementation status v0.1: codecs 0 (store), 1 (zstd, window sized to the block), 2 (zstd + trained dictionary, granular mode), 3 (xz/LZMA2 with a block-sized dictionary), 4 (PPMd variant H with the 7z range coder; parameters `[order, mem_size]` stored in `p`); `--codec best` tries zstd and xz per block (and PPMd when the block looks like text) and keeps the smallest. A block that zstd cannot shrink by at least 3 % is treated as incompressible: the slower codecs are skipped and the block is stored.

**Delta coding** (codec 5): a writer that recognises an entry as a new version of content it already holds — the same path in a reference archive, or a similarly named earlier entry of the same archive (same extension, common name prefix of at least half the name) — compresses that entry's blocks with zstd using the earlier content as a raw-content dictionary and keeps the result when it is smaller than the best ordinary encoding. The dictionary is described by chunk indices (`d`): chunks of a reference archive are registered in the chunk table as external (`x = 1`) even when no entry uses them directly, so the Merkle root covers them and readers resolve them through `--ref`. Rules: a dictionary chunk is never itself part of a delta blob (no decoding chains), dictionaries are at most 64 MiB (writers use 4 MiB), blocks never mix chunks with different dictionaries, the AEAD binds the codec id. A candidate is confirmed only when at least a quarter of the new bytes are shared with it (judged on 8 KiB content-defined pieces), and it is skipped when the earlier content already sits in the current block with room for the new entry (the codec's own window covers it then). Measured: corpus B (10 documents + 5 edited versions) 48.1 % → 45.2 % in block mode with 1 MiB random access (solid mode unchanged at 44.3 %), and the incremental archive of B against A's archive 6.8 % → 2.8 % of the input; unrelated inputs are unaffected.

**Pre-filters** (`f`, one byte: low nibble = filter id, high nibble = filter parameter) transform the block before the codec and are inverted after decoding. They are exactly invertible and never change the chunk identities (hashes are taken on the unfiltered bytes):

| filter id | name | parameter | use |
|---|---|---|---|
| 0 | none | – | default |
| 1 | x86 BCJ (E8/E9 branch converter, LZMA SDK `x86.c` semantics, state reset per block) | – | x86/x86-64 machine code; tried when a block shows ≥ 0.1 % `E8`/`E9` opcodes followed by a `00`/`FF` most-significant byte |
| 2 | ARM64 BCJ (`BL` / `ADRP` immediate conversion, XZ Utils `arm64.c` semantics) | byte offset (0–3) of the first aligned instruction word | AArch64 machine code; blocks start at arbitrary chunk boundaries, so the writer chooses the alignment with the most near `BL` calls |

Writers try the applicable filter and keep the smaller of the filtered and unfiltered encodings, so a filter can never make a block larger. Measured (`benchmarks/RESULTS-rust.md`; Windows x64 DLLs + exe, 13.5 MB): 32.5 % in block mode and 30.8 % solid with the filter, about one point worse without it (7-Zip BCJ2 29.7 %, WinRAR 33.1 %); ARM64 shared libraries (22.5 MB): 17.8 % block / 16.0 % solid (7-Zip 16.4 % with its automatic ARM64 filter, 15.9 % without, WinRAR 18.5 %). Reserved for a later revision: section-aware filtering driven by PE/ELF/Mach-O headers (filter only the code sections) and BCJ2-style split streams.

Modes:
- **granular**: one chunk per blob (random access per chunk, P2P-friendly, best for dedup and partial fetch). Ratio loss vs solid is compensated by dictionaries.
- **solid**: consecutive unique chunks grouped into blocks (default 4 MiB; text-only blocks may be larger). Random access at block granularity. Best ratio.
- Writers may mix: solid blocks for text, granular for large binaries.

### 4.3 Dictionaries and reference sets
- A **dictionary** is a zstd dictionary (trained with COVER/FastCover) stored inline or referenced by hash.
- A **reference set** is a content-addressed list of chunk IDs (and optionally a dictionary) that the receiver is assumed to possess: well-known corpora, a previous archive version, an organisation's "context pack", the Brotli-style static dictionary. The archive stores only the IDs; missing chunks are fetched (P2P/HTTP) or the archive is declared `incomplete` for that entry.
- This is the formal version of "compression by reference": the information transmitted is `new chunks + references`, exactly like CRAM stores reads against a reference genome, or Compression Dictionary Transport (RFC 9842, `dcb`/`dcz`) on the web, where a previous version used as dictionary cuts a 272 KB bundle to ~2.6 KB. Information-theoretically this is Slepian–Wolf: with side information Y at the decoder the rate needed is H(X|Y).
- Security note: never compress secret and attacker-controlled data against the same dictionary in an observable channel (CRIME/BREACH class leaks); reference sets are per trust domain.

---

## 5. Layer L1 — objects

### 5.1 Streams
`stream = [chunk_idx, ...]` (indices into the chunk table; varint-packed). Total length and BLAKE3 of the whole stream are recorded, so an agent can verify a file without re-hashing chunks individually (the chunk table is itself Merkle-committed).

### 5.2 Container recipes (container awareness)
Already-compressed containers are opened and their members chunked as plain data, plus a *recipe* to rebuild them:
- `zip` (DOCX/XLSX/PPTX/JAR/APK/EPUB…): a strict framing parser keeps the exact bytes of every local header, central-directory record, data descriptor and the end-of-central-directory block; each member payload is classified as `stored` (plaintext), `deflate` (inverted with **preflate** — `microsoft/preflate-rs`, Apache-2.0 — into plaintext plus a small corrections blob that recreates the stream bit-exact whatever encoder produced it) or `raw` (encrypted members, unusual methods, streams preflate cannot model). The rebuild is verified against the original hash at pack time; ZIP64, multi-disk or non-contiguous layouts fall back to storing the whole file `raw`. Measured: a DOCX written by Python 3.14 (zlib-ng) yields 928 KB of chunkable XML with 2.3 KB of corrections; Precomp-style results show silesia.zip going from 99.7 % (7-Zip) to 69.7 % once deflate is inverted.
- `pdf`: object streams expanded (Flate via preflate for bit-exactness); `/DCTDecode` JPEG streams and standalone `.jpg` files are recompressed losslessly with **Lepton** (segment kind 2: the chunked payload is the Lepton stream, the trailer records the original JPEG length; single-threaded encoding for determinism; ~22 % smaller); reconstruction verified against the original hash, else fallback to `raw`. Implemented in v0.1 (`explode_streams`).
- `tar`, `gzip`, `png` (IDAT) follow the same pattern.

### 5.3 References (external)
`{"ref": {"hash": <chunk id>, "size": n, "hints": ["https://…", "magnet:?xt=urn:btmh:…", "ipfs://bafy…", "tsaurref://<set-name>"]}}`. Hints are advisory; the hash is the identity.

Implementation status v0.1 (**reference archives**): a chunk-table row carries `x = 1` when the chunk is not stored in this archive but is held by a *reference archive*; the manifest lists `refs: [{root, hint, chunks, bytes}]`. `pack --ref base.tsr` registers every chunk of `base.tsr` as available and stores only the rest; readers opened with the same `--ref` resolve external chunks by hash (verified again on read), and report — never silently skip — chunks that no reference provides. On the benchmark corpora, packing the 15-file version set against the 10-file base archive stores only the changed chunks (see README).

### 5.4 Delta streams
A stream may be expressed as `base_stream + patch` where `base_stream` is another entry or external reference. Used for successive versions of the same document.

---

## 6. Layer L2 — manifest

Canonical CBOR map (JSON projection shown for readability):

```json
{
  "tsaur": 1, "profile": "documents", "created": "2026-09-22T19:00:00Z",
  "root": "b3:…",                                  // Merkle root of the chunk table
  "chunking": {"algo": "fastcdc", "min": 16384, "avg": 65536, "max": 262144},
  "limits": {"max_total_uncompressed": 4294967296, "max_entry": 1073741824, "max_ratio": 1000},
  "fidelity": "bit-exact | canonical | hybrid",
  "dicts": [{"id": 0, "hash": "b3:…", "size": 112640, "inline": true}],
  "refsets": [{"name": "org-context-2026Q3", "hash": "b3:…", "count": 1200, "hints": ["…"]}],
  "entries": [
    {
      "id": 0, "path": "raport-A.docx", "size": 57739, "hash": "b3:…", "mtime": "…",
      "media_type": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      "mode": "container:zip", "streams": {"word/document.xml": [12, 13, 14], "…": []},
      "derivatives": [
        {"kind": "canonical/markdown", "stream": [40, 41], "size": 63385, "generator": "tsaur-docx2md@0.1", "fidelity": "semantic"},
        {"kind": "summary", "text": "Raport tehnic despre …", "generator": "model:b3:…", "tokens": 120},
        {"kind": "embedding", "model": "b3:…", "dims": 1024, "dtype": "int8", "stream": [42]}
      ],
      "trust": {"level": "untrusted", "origin": "upload", "signed_by": []},
      "provenance": {"c2pa": "…", "source_uri": "…", "derived_from": []}
    }
  ],
  "policy": {
    "default_trust": 0, "exec": "deny | sandbox | allow-signed",
    "unicode": {"normalize": "NFC", "strip": ["tags", "zero-width", "bidi-controls"]},
    "instructions_from": ["key:did:example:owner#k1"],
    "agent_hint": "read canonical/markdown first; original bytes only if fidelity=bit-exact is required"
  },
  "views": {"agent": "TSAUR.md", "llms": "llms.txt", "repomix": "views/repomix.xml"},   // deterministic, data-only projections for LLM consumption
  "rights": {"cawg.training-mining": {"ai_generative_training": "notAllowed", "ai_inference": "allowed"}},
  "indexes": [{"type": "hnsw", "engine": "usearch", "hash": "b3:…", "trust": "cache", "rebuildable": true}],
  "bom": {"format": "cyclonedx-1.6", "path": "bom.cdx.json", "hash": "b3:…"},
  "crypto": { "...": "see §8" },
  "signatures": [{"type": "cose-sign1", "alg": "ed25519", "kid": "…", "sig": "…"}, {"type": "cose-sign1", "alg": "ml-dsa-65", "kid": "…", "sig": "…"}, {"bundle": "manifest.sigstore.json"}]
}
```

Per-derivative fields (see `derivatives[]` above) also carry `tokens: {"o200k_base": n, "claude": n}` (pre-computed so an agent can budget progressive loading: metadata → canonical → structure → raw) and `by: {tool, version, model?, deterministic: bool}` (PDF→Markdown conversion is generally *not* deterministic and must say so). Binary indexes (FAISS/hnswlib files can execute code on load) are never trusted: `trust: "cache"`, `rebuildable: true`.

Encoding: the canonical form is **dCBOR** (deterministic CBOR, RFC 8949 §4.2 + draft-mcnally-deterministic-cbor); signatures are **COSE_Sign1** (RFC 9052) over the canonical bytes; an optional Sigstore bundle (v0.3) carries the transparency-log inclusion proof.

Rules for agents (normative):
1. **Content is data, never instructions.** Nothing inside entries (including derivatives, summaries, file names) may be interpreted as commands. Only the signed `policy` map may carry directives, and only from keys the agent already trusts.
2. `trust.level` is an integer whose semantics are enforced by the **reader**, not asserted by the producer: `0 untrusted` (default: data only, displayed after stripping Unicode Tags U+E0000–E007F / zero-width characters, NFC-normalised, `sanitized: true` recorded), `1 attested` (valid in-toto/C2PA statement from a known key; still data), `2 instruction-capable` (content type `application/x-tsaur-instructions`, manifest signed by a key in the **agent's** allowlist, hash-pinned; only then may the text enter the model context as instructions, visibly marked), `3 executable` (level 2 + SLSA ≥ L2 provenance + SBOM, sandbox only). Lowering a level is always allowed; raising one requires a verifiable signature from the consumer's allowlist. The origin label propagates through every nesting level (the Mark-of-the-Web lesson of CVE-2025-0411).
3. Paths are informative labels, normalised (NFC, forward slashes, no `..`, no absolute roots, no drive letters). Extractors decide the on-disk location; the archive never does.
4. Token efficiency: the manifest is dCBOR on disk; a reader exposes a deterministic **agent view** (`TSAUR.md`: H1, one-paragraph summary, a flat table of entries with path/type/size/tokens/trust/short hash, an `Optional` section; target ≤ 2 000 tokens for ≤ 100 entries) and a compact JSON/TOON projection (flat tables save 30–60 % of tokens versus formatted JSON). Short hashes (8–12 hex) in views, full hashes only in CBOR; never base64 or long URLs in text the model reads; the view contains data and pointers only, never instructions.
5. Protocol bindings: an T-saur server exposes entries as MCP resources `tsaur://<id>/<path>?view=canonical|structure|raw` (`mimeType`, `size`, `annotations{audience, priority, lastModified}`, `ttlMs/cacheScope` per the 2026-07-28 MCP revision; `blob` only on explicit request) and as A2A 1.0 `Artifact{parts: [DataPart(manifest excerpt), FilePart{uri: tsaur://…}]}` with `metadata.trust` propagated. Agent Skills / AGENTS.md entries are accepted only when signed by a key in `policy.instructions_from`.

---

## 7. Layer L3 — semantic layer (optional)

Implementation status: canonical views are generated on demand (`read --view canonical`: DOCX → Markdown from the document XML, PDF → text) with a provenance header, and stored at pack time with `--canonical` (see §11, hybrid fidelity); exact token counts (a tokenizer instead of the chars/3.5 estimate) remain future work. The `tsaur mcp` server exposes the agent operations over the Model Context Protocol (stdio JSON-RPC): tools `tsaur_info`, `tsaur_list`, `tsaur_stat`, `tsaur_read`, `tsaur_grep`, `tsaur_diff`, `tsaur_verify`, `tsaur_unpack`, resources `tsaur://<archive file name>/<entry path>`, archives restricted to declared roots, every tool description labelling archive content as untrusted data; dual-era protocol support (2026-07-28 per-request versioning with `server/discover`, and the legacy `initialize` handshake). Citation identifiers are `tsaur://<merkle-root-hex>/<entry path>` with optional fragments `#L<a>-<b>` (lines) or `#B<start>-<end>` (bytes).

- **Canonical derivatives**: Markdown (text documents), JSON (tables/structured), plain text (OCR/transcripts). Generated by declared, versioned converters; fidelity always labelled `semantic`.
- **Summaries** (short, model-generated, model hash recorded).
- **Embeddings**: per chunk or per derivative section, model hash + dims + dtype (int8/binary recommended; a 1024-d int8 vector is 1 KiB); stored as a stream so they can be skipped.
- **Knowledge graph**: optional JSON-LD triples extracted from content (subject, predicate, object, evidence span).
- **Index**: optional HNSW/usearch serialisation for local search; must be reproducible from the embeddings.

The semantic layer is what lets an agent answer "what is in this archive and which 3 chunks do I need?" without downloading the body. It is also where *semantic dedup* lives (near-duplicate documents referenced instead of stored).

---

## 8. Cryptography

| Purpose | Primitive | Parameters | Notes |
|---|---|---|---|
| Content hash / Merkle | BLAKE3-256 | plaintext chunk IDs; in encrypted archives IDs are `BLAKE3.keyed_hash(K_content_id, chunk)` | tree hash, verified streaming (Bao); keyed IDs prevent "confirmation of a file" while keeping dedup inside one key domain |
| Transport Merkle (verify-cap) | BLAKE3 over the **ciphertext** pieces (16 KiB leaves, BEP 52 layout) | root signed in the manifest | peers and relays verify and repair without being able to decrypt (Tahoe-LAFS verify-cap) |
| Header integrity | HMAC-SHA-256 or BLAKE3 keyed, key = HKDF(DEK, "tsaur/v1/header") | covers the cleartext header (algorithms, KDF params, recipient stanzas) | no cheap password verifier (WinZip 2 B / RAR5 12 B anti-pattern): the password is checked only through this tag after the full KDF |
| Chunk/blob AEAD | XChaCha20-Poly1305 (default) or AES-256-GCM (HW-accelerated option) | nonce = HKDF(DEK, "nonce", blob_index) (24 B), AAD = blob header ‖ manifest root | chunked STREAM-style construction (Cryptomator/age model): each blob authenticated, index-bound and archive-bound; final blob flagged |
| Manifest / index | same AEAD, separate derived subkeys | AAD = section header | metadata (names, sizes) are encrypted |
| Data key | DEK, 256-bit random per archive | — | never derived from a password directly |
| Password KDF | Argon2id (RFC 9106) | default m=256 MiB, t=3, p=4; reader-enforced floor m=64 MiB, t=3; salt 16 B; parameters in header | wraps a KEK; the passphrase stanza is exclusive (age rule); PBKDF2 forbidden; scrypt N=2^17 only where Argon2 is unavailable |
| Recipients | X25519 + ML-KEM-768 hybrid KEM (per recipient, age-style stanza `x25519mlkem768`) | secret = KDF(ss_mlkem ‖ ss_x25519 ‖ ek ‖ ct ‖ context) (RFC 10024 / SP 800-227 combiner) | "harvest now, decrypt later" defence; multiple recipients per archive; other stanzas: `argon2id`, `fido2-hmac`, `kms`, `shamir(t,n)` |
| Signatures | composite Ed25519 **and** ML-DSA-65, over the sections table hash and manifest root | — | dual signature per NIST IR 8547; SLH-DSA-128s for long-lived root keys; valid only if *both* verify when `strict-pq` flag is set |
| Timestamps / anchoring | Rekor (Sigstore) entry or OpenTimestamps proof of `root` | optional | "DLT" usage limited to anchoring hashes, never storing data |
| Padding | Padmé-style length padding for blobs and sections when `hide-sizes` flag set | — | reduces size leakage |
| Convergent mode | optional, per reference set: key = HKDF(secret, chunk hash) | — | only for org-internal dedup; documented "confirmation-of-file" risk |

Key hierarchy:

```
password ──Argon2id──▶ KEK_pw ─┐
recipient X25519+ML-KEM ──────▶ KEK_i ─┼── wraps ──▶ DEK (archive) ──HKDF──▶ K_blob, K_manifest, K_index, K_nonce
hardware key (FIDO2 hmac-secret)▶ KEK_h ┘
```

Implementation status v0.1: the recipients section is a CBOR list of stanzas, each wrapping the same archive key — `argon2id` (salt, m, t, p) and `x25519mlkem768` (ephemeral X25519 public key, ML-KEM-768 ciphertext; KEK = HKDF-SHA-256(ss_mlkem ‖ ss_x25519, salt = eph ‖ BLAKE3(ct), info "tsaur v1 x25519mlkem768")); identities are 96 bytes (X25519 secret + ML-KEM seed), recipient keys 1216 bytes. Composite ML-DSA signatures and FIDO2 stanzas are not implemented yet.

Key rotation = re-wrapping the DEK (rewrite of the small recipient block only). Revocation of a recipient requires re-encryption with a new DEK (documented limitation; convergent chunks are not re-encrypted).

---

## 9. Safety limits (normative)

- Readers MUST enforce `limits` from the manifest **and** their own ceilings (default 4 GiB total, 1 GiB per entry, ratio ≤ 1000×) before allocating; exceeding → abort with `E_BOMB`. Reference implementation ceilings: 64 GiB total, 1 GiB per structured section, 64 MiB blob cache, 256 MiB preflate plaintext, 64 MiB Lepton input, 1 GiB xz decoder memory, 1 GiB PPMd model.
- Unknown codec ids, filter ids, stanza types or section types in a version-1 archive MUST be rejected with a clear error (`E_UNSUPPORTED`, CLI exit code 7 / corrupt 2), never skipped.
- Nested containers are expanded at most 2 levels deep; recipes cannot reference themselves (DAG check).
- Paths are labels only (§6.3). Symlinks/hardlinks are never created by default.
- Any AEAD failure aborts the whole extraction (no partial plaintext output).
- Reference implementation in a memory-safe language (Rust); every parser of external bytes (archive, volumes, descriptors, transfer requests and replies, resume files) is exercised with systematically damaged inputs on the stable toolchain (`tests/robustness.rs`, campaign length and iteration counts reported in `docs/review/RC1-VERIFICATION-REPORT.md`); cargo-fuzz targets on nightly remain planned. The xz-utils backdoor (2024) motivates minimal dependencies and reproducible builds.

---

## 10. Layer L4 — distribution and recovery

- **Pieces**: the archive file is cut into fixed pieces (default 256 KiB; 64 KiB for small archives). `pieces` sidecar = `{piece_size, [BLAKE3 per piece], merkle_root, sha256 layer (optional, BitTorrent v2 compatible)}`. Implementation status v0.1: `<archive>.pieces` is CBOR `{file, size, piece_size, pieces: [h…], parity: [h…], stripes: [{first, k, m, par_off}…], root}`, piece size 1 KiB–16 MiB, the final piece zero-padded for hashing; `<archive>.par` holds the parity symbols stripe after stripe. Stripes hold at most 200 data pieces and at most 64 MiB of data, so creation, verification and repair stream one stripe at a time whatever the archive size. Readers validate the stripe table (contiguous, within bounds, k + m ≤ 256, one parity hash per parity piece) before use; a file whose length differs from `size` has its last piece flagged so that repair also restores the exact length; a stripe damaged beyond its parity is reported while the other stripes are still repaired.
- **Offline volume sets** (implemented, `docs/design/VOLUME-SETS.md`): an archive file is striped across `N` data volumes plus `M` parity volumes (`.tsrv` files, one piece per volume per stripe, systematic Reed-Solomon over GF(2^8), `N + M <= 256`), so that it can be spread over directories or removable drives and rebuilt bit-exact from **any `N` of the `N + M` volumes**. Every volume is self-describing: a 64-byte header (`TSV\x1A`, version, volume index, `N`, `M`, piece size, archive size, set id, payload length), the pieces, the CBOR descriptor (set id, archive name, size and BLAKE3, geometry, the BLAKE3 of every data and parity piece, Merkle root, generator) and a 56-byte trailer (descriptor offset/length/BLAKE3, volume index, `VST\x1A`). The descriptor is identical in every volume, so no separate index file exists; a volume is identified by its header or its trailer, whichever survives. The set id is derived from the archive hash and the geometry, so splitting twice or repairing a lost volume gives byte-identical files. `join` writes the archive to a temporary name and renames it only after its BLAKE3 matches the descriptor. Volumes carry the archive bytes as they are: an encrypted archive stays encrypted, and the descriptor exposes only the archive file name, size and hashes. Default piece size: adaptive (the smallest power of two from 64 KiB to 1 MiB giving each data volume at least 16 pieces; the recorded size is authoritative, readers never apply the rule). Trust contract for descriptors and pieces received from elsewhere: `docs/design/VOLUME-TRUST.md` (the set id authenticates the archive hash and geometry, the descriptor hash authenticates every piece hash; a hash never authenticates authorship). A prototype exchange protocol (`TSXP/1`, `docs/design/VOLUME-SETS.md` §8) transfers pieces between two instances with manually supplied addresses, over plain TCP or inside TLS 1.3 with self-signed certificates pinned by fingerprint on both sides; it is not part of the archive format.
- **Verified streaming**: BLAKE3/Bao allows verifying any 1 KiB slice against the root; a peer can serve arbitrary byte ranges.
- **Erasure coding**: default **systematic Reed-Solomon** recovery record (Leopard/ISA-L-class over GF(2^16); the prototype uses GF(2^8), ≤ 255 pieces per stripe) at configurable overhead (5–20 %, PAR2 default 5 %). Every parity symbol is BLAKE3-hashed and listed in the `pieces` sidecar, so parity is verifiable and distributable like any chunk (FEC alone never authenticates). **RaptorQ** (RFC 6330) is reserved for a later "fountain broadcast" mode. Parity lives in `.tsr.par` so the main file stays minimal.
- **Interop**: export as CARv2 (IPLD, `MultihashIndexSorted` index) with `blake3` multihash CIDs for internal identity and, for public IPFS, canonical `unixfs-v1-2025` sha2-256 CIDs (1 MiB fixed chunks) generated from the same import; publish as hybrid BitTorrent v1/v2 torrent (piece layers from the sha256 layer; hybrid swarms still default to v1 in most clients); map to an OCI artifact (each blob a layer, manifest as config) for registries. Primary agent-to-agent transport: **iroh / iroh-blobs** (BLAKE3 verified streaming, QUIC, NAT traversal, 1.0 since June 2026); BitTorrent DHT (BEP 5) remains the only neutral global discovery layer.
- **Peer hints & anchoring**: `hints` URIs; optional `anchors: [{"type": "rekor", "log_index": …}, {"type": "ots", "proof": …}]`.
- **Partial archives** are first-class: a `.tsr` may contain only some blobs; the manifest says which; agents fetch the rest by hash.

---

## 11. Fidelity modes

| mode | what is stored | restore guarantee | intended use |
|---|---|---|---|
| `bit-exact` | original streams (chunked, container-aware with verified recipes) | byte-identical, hash-verified | archival, legal, code |
| `canonical` | canonical derivatives only (Markdown/JSON/text) + provenance | information-equivalent, NOT byte-identical | agent working sets, RAG corpora, memory |
| `hybrid` | canonical derivatives inline + original streams as (possibly external) references | byte-identical if references resolvable | default for agent exchange |

Implementation status v0.1: `pack --canonical` produces fidelity `hybrid`: every original is stored bit-exact as before, and for each DOCX/PDF entry a derived entry `.tsaur/views/<path>.md|.txt` holds the canonical view generated at pack time, with `derived: {from, view, generator, tokens_est}` in the manifest (`tokens_est` = chars / 3.5, a planning estimate). Views are chunked, deduplicated, compressed, encrypted and verified like any entry; `read --view canonical` serves the stored view (and converts on demand when none is stored); default extraction skips views unless they are selected explicitly; a view that cannot be generated leaves a `note` on its source entry and never blocks the original. Cost on corpus A: 62.2 % → 65.4 % of the input for 4 views (281 KB of Markdown/text, ≈ 80 k tokens pre-computed). Stored embeddings and summaries remain future work.

Prototype numbers (`benchmarks/RESULTS.md`, 10 mixed files, 1.54 MB): bit-exact solid 73.1 % of original (7-Zip LZMA2 ultra 73.6 %, WinRAR 7.23 RAR5 solid 74.2–74.4 %, brotli 73.2 %); canonical 11.1 % — the only mode that "breaks" the PDF/DOCX barrier, because it stores information rather than bytes.

---

## 12. Versioning and extensibility

- `version` in header = major; manifest `tsaur` = schema version. Unknown sections/keys MUST be ignored (readers) and preserved (rewriters).
- **v1 readers accept exactly `version = 1`** in `.tsr` headers and `.tsrv` volume headers. Any other value is refused with "unsupported format version N" (CLI exit code 2), never read with v1 rules: a future major version may change anything after the magic and the version field. For `.tsrv` this holds even when the trailer of the file is intact (a volume whose header names another version is not treated as a damaged v1 volume).
- **Writer freeze.** The reference writer records `tsaur-core/1.0` as generator in manifests, derived views and volume descriptors; the same inputs, options and build produce byte-identical archives in every 1.x release (`tests/golden.rs` compares the writer's output with the committed golden archives). A change of those bytes is a format event: it needs a new generator string, regenerated golden archives, a CHANGELOG entry and compatibility tests, and it may not make v1 readers misread anything.
- **Readers stay backward compatible within v1**: every archive written by any 1.x release opens in every later 1.x reader; entries that need an optional reader feature (today only Lepton segments, `requires: lepton`) are reported by name, never silently skipped.
- Codec ids ≥ 128 are private; ≥ 64 are experimental (registered in `docs/spec/registry.md`). Filter ids 8–15 are private (the high nibble of `f` is the parameter, so at most 16 filter ids exist in v1; a later revision may add a `filters` array for chains).
- Framing (header, trailer, section table with CRC-32, BLAKE3 per section) and the recipients and signature sections are readable **without credentials**, so a reader can report how an archive can be unlocked (`stanzas`) and by which key it was signed before asking for a passphrase (`tsaur info` on a locked archive).
- Reserved: journaling/append (v0.2), multi-archive "constellations" (v0.3), streaming pack over the network.

---

## 13. Open questions (to decide before v0.2)

1. Name and extension: the former working name (`.aix`) conflicted with IBM's live AIX® trademark (class 9) and was taken on PyPI/npm/crates.io/GitHub. Decision (founder, 2026-09-22): **T-saur** — packages `tsaur` (free on PyPI/npm/crates.io), GitHub org `tsaur-format`, extension `.tsr`, media type `application/vnd.tsaur+cbor` (to register). Magic bytes will become `TSR\x1A` before the first public release; formal trademark search still pending.
2. Default chunk profile for mixed document sets: 64 KiB (P2P dedup) vs 256 KiB (less metadata) — measure on larger corpora.
3. Whether the semantic index is inline by default (bigger archive) or sidecar (simpler streaming).
4. Model-predictive codec: which small open model to pin first (RWKV/Qwen-class, ≤ 1 B params, integer inference) and how to guarantee determinism across CPUs.
5. Hybrid signatures: mandatory ML-DSA from day one, or optional until PQ libraries stabilise.
