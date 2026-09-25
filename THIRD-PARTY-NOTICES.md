# Third-party notices

T-saur itself is licensed Apache-2.0 OR MIT (`LICENSE-APACHE`, `LICENSE-MIT`). The release binaries
statically link the Rust dependencies listed in `tsaur/Cargo.lock`. Their licenses, as declared in
their crate metadata (`cargo metadata`, 246 packages on 2026-09-24, after the TLS transport was added):

| License (as declared) | Packages |
|---|---:|
| MIT OR Apache-2.0 (and spelling variants) | 179 |
| MIT | 25 |
| BSD-3-Clause, BSD-2-Clause, BSD-1-Clause (alone or in combinations) | 15 |
| Apache-2.0 | 6 |
| Zlib (alone or in OR-combinations) | 6 |
| ISC, alone or with Apache-2.0 / MIT (`rustls`, `rustls-webpki`, `untrusted`, `ring`) | 4 |
| Unlicense OR MIT | 3 |
| CC0-1.0 / MIT-0 combinations | 2 |
| Apache-2.0 WITH LLVM-exception combinations | 2 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later (`r-efi`; used under MIT) | 1 |
| **LGPL-3.0-or-later (`cabac`)** | 1 |

The encrypted transport (`tsaur volumes serve/fetch` with `--tls-identity`) adds `rustls`
(Apache-2.0 OR ISC OR MIT), `rustls-webpki` (ISC), `rustls-pki-types` (MIT OR Apache-2.0),
`rcgen` (MIT OR Apache-2.0) and `ring` (declared `Apache-2.0 AND ISC`: ISC for its own code,
Apache-2.0 for the parts taken from BoringSSL, plus once_cell under MIT OR Apache-2.0; its
`LICENSE`, `LICENSE-BoringSSL` and `LICENSE-other-bits` files carry the notices that a binary
distribution must reproduce). All of these are permissive and are part of every build.

## The LGPL component

`cabac` (context-adaptive binary arithmetic coding) is a dependency of `preflate-rs`, the library
that inverts deflate streams so that ZIP/OPC members and PDF `FlateDecode` streams can be
recompressed and rebuilt bit-exact. It is licensed **LGPL-3.0-or-later** (the texts are in
`licenses/LGPL-3.0.txt` and `licenses/GPL-3.0.txt`, which the LGPL incorporates by reference).

* **It is part of every build.** `preflate-rs` is not optional in `tsaur-core`, and every published
  version of `preflate-rs` depends on `cabac`; `cargo tree -i cabac` shows the path in the default
  build and in the lite build alike. (`lepton_jpeg` 0.5.8, the JPEG recompressor of the full build,
  no longer depends on `cabac`.) Earlier versions of this file, the distribution policy and the
  README said that the lite build contains no LGPL code; that was wrong and was corrected on
  2026-09-25 after a dependency audit. The lite build differs from the full build only by the
  absence of Lepton JPEG recompression.
* When a binary that contains `cabac` is distributed, the LGPL-3.0 obligations for that component
  apply: prominent notice (this file), the license texts, and — because the library is statically
  linked — the Corresponding Application Code and the Minimal Corresponding Source needed to relink
  a modified `cabac` into the application (LGPL-3.0 §4). T-saur's complete source, its pinned
  dependency versions (`Cargo.lock`) and the build instructions (`CONTRIBUTING.md`) provide that:
  anyone can obtain the sources with `cargo vendor` and rebuild the binary with a modified library.
* The T-saur format does not depend on this library: a Lepton segment is a standard Lepton stream,
  for which Apache-2.0 implementations exist. Only the Rust reference implementation's default
  build uses `cabac`.

This is a description of the project's understanding of its obligations, not legal advice; a
distributor who wants certainty should confirm it with counsel.

## How this list is produced

```bash
cd tsaur
cargo metadata --format-version 1 | python -c "import json,sys,collections;m=json.load(sys.stdin);ws=set(m['workspace_members']);c=collections.Counter((p.get('license') or 'UNKNOWN') for p in m['packages'] if p['id'] not in ws);print(*sorted(c.items(),key=lambda x:-x[1]),sep=chr(10))"
```
