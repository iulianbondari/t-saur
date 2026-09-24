# Benchmark results (generated automatically by `prototype/bench.py`)

Machine: Windows 11, Python 3.14.7, zstandard 0.25.0, zstd lib (1, 5, 7); 7-Zip 26.03 (found) and WinRAR 7.23 (found) run through their CLI. `xz -9e` = LZMA2 from Python; all measurements are single-run, the times include the start-up of the external processes.

## 1. Corpus A: 10 random files (.md/.txt/.docx/.pdf), 1,538,928 bytes

### 1.1 Per file (what compresses and what does not)

| File | Original | zstd -19 | xz -9e | % (xz) | Note |
|---|---:|---:|---:|---:|---|
| brochure-B.pdf | 877,098 | 873,488 | 874,072 | 99.7% | Flate streams + images |
| changelog-node-gyp.md | 118,645 | 26,231 | 24,960 | 21.0% |  |
| idle-news.txt | 59,342 | 21,105 | 20,672 | 34.8% |  |
| paper-A.pdf | 76,110 | 64,940 | 65,144 | 85.6% | Flate streams |
| python-docs-part1.txt | 200,424 | 52,307 | 51,700 | 25.8% |  |
| python-license.txt | 34,824 | 9,558 | 9,412 | 27.0% |  |
| readme-encoding_rs.md | 38,133 | 12,600 | 12,484 | 32.7% |  |
| readme-semver.md | 24,760 | 7,622 | 7,596 | 30.7% |  |
| report-A.docx | 57,726 | 55,085 | 55,504 | 96.2% | ZIP container (already Deflate) |
| report-B.docx | 51,866 | 49,214 | 49,568 | 95.6% | ZIP container (already Deflate) |

### 1.2 Classic formats (measured locally)

| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |
|---|---:|---:|---:|---:|---|
| zip (deflate -9, per file) | 1,189,268 | 77.3% | 1.29x | 0.03 | per file |
| zip (bzip2 -9, per file) | 1,177,817 | 76.5% | 1.31x | 0.11 | per file |
| zip (lzma, per file) | 1,179,429 | 76.6% | 1.30x | 0.30 | per file |
| tar + gzip -9 | 1,191,070 | 77.4% | 1.29x | 0.03 | solid |
| tar + bzip2 -9 | 1,167,098 | 75.8% | 1.32x | 0.09 | solid |
| tar + xz -9e (LZMA2, ~7z ultra) | 1,133,460 | 73.7% | 1.36x | 0.28 | solid |
| tar + brotli -q 11 -w 24 | 1,126,606 | 73.2% | 1.37x | 2.09 | solid |
| tar + zstd -19 | 1,136,240 | 73.8% | 1.35x | 0.21 | solid |
| tar + zstd --ultra -22 --long=27 | 1,136,190 | 73.8% | 1.35x | 0.26 | solid |
| 7-Zip 26.03: 7z LZMA2 -mx9 solid | 1,132,927 | 73.6% | 1.36x | 0.10 | solid |
| 7-Zip 26.03: 7z PPMd -mx9 (o32, 1 GB) solid | 1,147,026 | 74.5% | 1.34x | 0.24 | solid |
| 7-Zip 26.03: zip Deflate -mx9 | 1,182,253 | 76.8% | 1.30x | 0.21 | solid |
| WinRAR 7.23: RAR5 -m5 solid -md256m | 1,144,320 | 74.4% | 1.34x | 0.13 | solid |
| WinRAR 7.23: RAR5 -m5 solid -md256m -mcx (alt. search) | 1,142,511 | 74.2% | 1.35x | 0.14 | solid |
| WinRAR 7.23: RAR5 -m5 solid + recovery record 10% | 1,292,230 | 84.0% | 1.19x | 0.13 | solid |
| WinRAR 7.23: RAR5 -m5 NON-solid | 1,182,003 | 76.8% | 1.30x | 0.26 | solid |

### 1.3 T-saur prototype (lossless bit-exact) - variants

| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |
|---|---:|---:|---:|---:|---|
| T-saur v0 (py): chunk 8K + zstd dict, per-chunk | 1,214,946 | 78.9% | 1.27x | 6.69 | chunk+dedup |
| T-saur v0 (py): chunk 8K, NO dict, per-chunk | 1,230,147 | 79.9% | 1.25x | 5.34 | chunk+dedup |
| T-saur v0 (py): chunk 64K avg + dict, per-chunk | 1,164,226 | 75.7% | 1.32x | 3.28 | chunk+dedup |
| T-saur v0 (py): solid blocks 1 MiB (best-of zstd/xz), container-aware | 1,124,027 | 73.0% | 1.37x | 1.87 | chunk+dedup |
| T-saur v0 (py): solid 1 MiB, container-aware OFF | 1,142,380 | 74.2% | 1.35x | 0.70 | chunk+dedup |
| T-saur v0 (py): solid 1 MiB + XChaCha20-Poly1305/Argon2id encryption | 1,124,409 | 73.1% | 1.37x | 2.22 | chunk+dedup |

Dedup/chunking details (per-chunk 8K + dict variant):
- input: 1,538,928 bytes of original files -> 3,263,889 logical bytes after container expansion (DOCX -> XML); chunks: 349 total, 260 unique; internal dedup (repetitive XML, repeated images in PDF): 824,612 bytes; dictionary: 95,815 bytes; manifest (CBOR+zstd): 13,526 bytes; codecs chosen: {'zstd19+dict': 259, 'xz9e': 1}; bit-exact roundtrip: True; unpack 0.02s
- solid: 3 blocks; codecs: {'xz9e': 3}; dictionary: 0 bytes (disabled in solid mode); manifest 11,051 bytes; roundtrip: True

### 1.4 Already-compressed containers: how much expansion gains + the canonical representation for agents

| File | Original | Expanded container | xz(expanded) | Canonical (md) | xz(canonical) | Bit-exact? |
|---|---:|---:|---:|---:|---:|---|
| brochure-B.pdf | 877,098 | 1,018,102 | 748,836 | 40,340 | 9,808 | no (requires preflate) |
| paper-A.pdf | 76,110 | 281,307 | 34,488 | 120,591 | 30,668 | no (requires preflate) |
| report-A.docx | 57,726 | 928,501 | 38,916 | 63,379 | 18,860 | yes (deflate level 6) |
| report-B.docx | 51,866 | 906,052 | 33,204 | 56,609 | 14,064 | yes (deflate level 6) |

### 1.5 'agent/canonical' mode (DOCX/PDF -> Markdown; semantic-lossless, NOT bit-exact)

| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |
|---|---:|---:|---:|---:|---|
| T-saur v0 (py) canonical: docx/pdf -> md, solid 1 MiB | 171,285 | 11.1% | 8.98x | 0.71 | chunk+dedup |

Canonical entries: brochure-B.pdf.md (877,098 -> 40,340 bytes text), paper-A.pdf.md (76,110 -> 120,591 bytes text), report-A.docx.md (57,726 -> 63,379 bytes text), report-B.docx.md (51,866 -> 56,609 bytes text)

## 2. Corpus B: the same 10 files + 5 edited versions (15 files, 2,192,934 bytes) - dedup scenario

### 2.1 Classic formats

| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |
|---|---:|---:|---:|---:|---|
| zip (deflate -9, per file) | 1,462,381 | 66.7% | 1.50x | 0.06 | per file |
| zip (bzip2 -9, per file) | 1,428,234 | 65.1% | 1.54x | 0.17 | per file |
| zip (lzma, per file) | 1,429,095 | 65.2% | 1.53x | 0.46 | per file |
| tar + gzip -9 | 1,461,549 | 66.6% | 1.50x | 0.05 | solid |
| tar + bzip2 -9 | 1,318,974 | 60.1% | 1.66x | 0.15 | solid |
| tar + xz -9e (LZMA2, ~7z ultra) | 1,208,376 | 55.1% | 1.81x | 0.42 | solid |
| tar + brotli -q 11 -w 24 | 1,199,296 | 54.7% | 1.83x | 1.85 | solid |
| tar + zstd -19 | 1,211,059 | 55.2% | 1.81x | 0.21 | solid |
| tar + zstd --ultra -22 --long=27 | 1,210,979 | 55.2% | 1.81x | 0.26 | solid |
| 7-Zip 26.03: 7z LZMA2 -mx9 solid | 1,206,834 | 55.0% | 1.82x | 0.13 | solid |
| 7-Zip 26.03: 7z PPMd -mx9 (o32, 1 GB) solid | 1,266,387 | 57.7% | 1.73x | 0.35 | solid |
| 7-Zip 26.03: zip Deflate -mx9 | 1,448,117 | 66.0% | 1.51x | 0.22 | solid |
| WinRAR 7.23: RAR5 -m5 solid -md256m | 1,220,071 | 55.6% | 1.80x | 0.14 | solid |
| WinRAR 7.23: RAR5 -m5 solid -md256m -mcx (alt. search) | 1,218,227 | 55.6% | 1.80x | 0.16 | solid |
| WinRAR 7.23: RAR5 -m5 solid + recovery record 10% | 1,375,581 | 62.7% | 1.59x | 0.15 | solid |
| WinRAR 7.23: RAR5 -m5 NON-solid | 1,440,546 | 65.7% | 1.52x | 0.12 | solid |

### 2.2 T-saur prototype

| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |
|---|---:|---:|---:|---:|---|
| T-saur v0 (py): chunk 8K + dict, per-chunk (CDC dedup) | 1,369,877 | 62.5% | 1.60x | 8.13 | chunk+dedup |
| T-saur v0 (py): solid 1 MiB (CDC dedup + solid) | 1,200,395 | 54.7% | 1.83x | 2.07 | chunk+dedup |

- CDC dedup: 521 chunks, 309 unique; 1,907,119 bytes removed before compression out of 4,792,900 logical (39.8%; original input 2,192,934); manifest 16,173 bytes; roundtrip: True

## 3. P2P pieces + Reed-Solomon recovery (sidecar .meta / .par)

- Encrypted archive (Argon2id + XChaCha20-Poly1305), 1,124,410 bytes, 64 KiB pieces: k=18 data pieces, m=2 RS parity pieces (131,072 bytes of parity = 11.7%), .meta = 820 bytes, Merkle root = `efe79c74fe993773...`
- Pieces deliberately corrupted: 2 ([0, 9]); repaired: 2; verification after repair: OK; bit-identical to the original: YES

## 4. How to read the numbers

- The corpus is ~57% PDF with images (brochure-B.pdf, 877 KB) - practically incompressible for any classic archiver; the texts (.md/.txt) compress 4-5x.
- 'Solid' (tar+xz/zstd) beats 'per file' (zip) because it uses the context across files; T-saur with solid blocks recovers this advantage and keeps dedup + block-level access.
- Per-chunk compression (8 KB) loses context; the trained dictionary recovers part of it (see the difference dict vs. no dict) - exactly the 'shared references' mechanism.
- CDC dedup matters only when there is real redundancy (versions, boilerplate); on 10 unrelated files the gain is ~0. zstd --long / RAR with a large dictionary also catch redundancy in solid mode, but without granular/P2P access.
- The canonical (agent) mode is the only one that 'breaks' the PDF/DOCX barrier: it stores the information (text+structure), not the bytes; it is explicitly declared non-bit-exact.
