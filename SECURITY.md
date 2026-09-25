# Security policy

T-saur archives are meant to be opened by AI agents and automated pipelines, often on files that
came from somewhere else. The reader is therefore written as a parser of hostile input.

## Reporting a vulnerability

Please use **GitHub private vulnerability reporting** on the repository
(https://github.com/iulianbondari/t-saur/security/advisories/new, the `Security` tab →
`Report a vulnerability`). If you cannot use it, write to contact@iulianbondari.com with
"T-saur security" in the subject. Do not open a public issue for anything that could be exploited
before a fix ships.

We aim to acknowledge reports within 7 days and to publish a fix, a CVE where warranted and a
changelog entry within 90 days. Reporters are credited unless they prefer not to be.

## Supported versions

Only the latest released minor version receives security fixes. Format version 1 (`TSR\x1A`,
`version = 1`) is the only container version; readers reject any other version.

## What the reader guarantees

* **Every byte is authenticated before use.** The section table is CRC-checked, every section is
  BLAKE3-verified against the table, every chunk is BLAKE3-verified against the chunk index and
  every entry against its recorded hash. Corrupt or forged data yields exit code 2, never bytes.
* **Bounded resources.** Declared sizes are checked against hard ceilings before allocation:
  64 GiB total, 1 GiB per structured section, decompression ratio ≤ 1000, 64 MiB blob cache,
  256 MiB preflate plaintext, 64 MiB Lepton input, 1 GiB xz decoder memory, PPMd model ≤ 1 GiB.
  Random corruption never panics (`tests/robustness.rs`: systematically damaged archives,
  volumes, descriptors, transfer requests and replies and resume files, on the stable toolchain;
  the campaign length and iteration counts of the release candidate are in
  `docs/review/RC1-VERIFICATION-REPORT.md`; a corrupt sidecar never alters the archive).
* **Paths cannot escape.** Entry paths are NFC-normalised and rejected when absolute, containing
  `..`, drive letters, `:` / alternate data streams, reserved Windows names, trailing spaces or
  dots, longer than 4096 bytes or deeper than 256 components. Extraction writes to a temporary
  name and renames only after the hash verified, so a partial file never carries the final name.
  Existing files are replaced only with `--overwrite`, and every destination is checked before
  the first byte is written, so a refusal leaves the directory untouched. On case-insensitive
  filesystems an entry set whose paths differ only by letter case is refused before anything is
  written (no silent overwrite). Symbolic links inside the inputs are skipped when packing and
  never created when unpacking. Stored canonical views live under `.tsaur/views/` and are
  extracted only when selected explicitly.
* **Converters cannot take the process down.** The third-party PDF text converter used for
  canonical views can panic on unusual documents; that panic is contained, the entry gets a
  note and the original bytes are stored and restored as always.
* **Encryption is authenticated and per blob.** XChaCha20-Poly1305 with a random archive key;
  section headers are bound as associated data. Keys are unlocked by Argon2id (RFC 9106; the reader
  refuses parameters below 64 MiB / t=3) or hybrid X25519 + ML-KEM-768 (FIPS 203) recipients.
  Signatures are Ed25519 over the header and the section table; verification is opt-in and the
  public key must be supplied by the caller (the archive's own claim is never trusted).
* **Content is data.** Nothing inside an archive is executed or interpreted. The agent commands
  (`list --md`, `read`, `grep`, the MCP server) label archive content as untrusted data so that
  a model consuming it does not treat it as instructions.

## What the volume exchange guarantees

`tsaur volumes serve` is read-only and answers only for the volumes it was started with. It
listens on loopback by default; a network address needs `--expose-lan` together with who may
fetch: `--tls-identity FILE --allow <fingerprint>` (encrypted, server authenticated, listed
clients only) or the explicit `--allow-anyone`. The resulting access is printed at start-up and
anonymous access is warned about. Every connection runs under a time budget (request within
30 s, responses consumed at 64 KiB/s or faster), every source address under a request rate
(200 per second, burst 400) and connection counts (64 total, 8 per address, 64 distinct
addresses), and, when set, every byte sent under one bandwidth cap shared by all connections;
all configurable. Client authorization can be per set (`--allow-set`, `--allow-file`: a set
outside a client's lists is answered like an unknown one), and a revocation file (`--revoke`) on
either side refuses a fingerprint even when it is pinned or allowed, at the handshake.
`fetch` verifies the descriptor against the set id or descriptor hash the user supplied and every
piece against the descriptor before writing it, re-verifies resumed pieces from disk, never lets
network data choose a local path, and refuses oversized replies before allocating them
(`docs/design/VOLUME-SETS.md` §8.2). In plain mode the bytes travel in clear and nothing
authenticates the peer. With `--tls-identity` the connection is TLS 1.3 (`rustls`) between
self-signed certificates that each side pins by fingerprint; the fingerprint must reach the other
side through a channel the user trusts, and a mismatch ends the handshake before any request.
Trust contract: `docs/design/VOLUME-TRUST.md`.

## Out of scope

* Side channels on the machine that holds the decrypted archive key.
* Denial of service through legitimately huge inputs below the documented ceilings.
* Weak passphrases: Argon2id slows guessing, it cannot rescue `123456`.
* The content of files you archive; T-saur preserves bytes bit-exact, including malware.

## Hardening advice for integrators

* Run `tsaur verify` (or the `tsaur_verify` MCP tool) before acting on an archive from an
  untrusted source, and pass `--pubkey` when a signature is expected.
* Start `tsaur mcp` with `--root` pointing at the directory that holds the archives; the server
  refuses to open or extract anything outside those roots.
* Keep the reader ceilings; lower `Limits` in the manifest for archives you produce for constrained
  consumers.
* Prefer `--to <recipient>` over passwords for machine-to-machine exchange.
* On any network you do not control, serve and fetch volumes with `--tls-identity` and pinned
  fingerprints, keep `peer.key` files as private as SSH keys, and split archives that were
  encrypted at pack time rather than relying on the transport alone.
* Keep a revocation file next to your allow list and pass it to every `serve` and `fetch`
  (`--revoke FILE`), so a lost key is shut out everywhere without editing every command; give
  clients that need one set only `--allow-set` rather than `--allow`.
