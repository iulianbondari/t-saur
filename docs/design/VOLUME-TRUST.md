# Trust contract for volume descriptors and pieces received from another machine

Status: normative for `tsaur volumes serve` / `fetch` (protocol `TSXP/1`) and for any later
peer-to-peer stage. Everything here is about *integrity against an expectation the receiver
already holds*. The current transport (plain TCP, manually supplied addresses) provides **no
confidentiality and no peer authentication** unless the TLS mode of §6 is used, in which case
the peer's identity is a certificate fingerprint the user pinned by hand.

## 1. Identities involved

| Identity | Where it comes from | What it covers |
|---|---|---|
| **archive hash** `archive_b3` | BLAKE3 of the whole `.tsr` file, computed at split time | the exact bytes of the archive |
| **set id** | `BLAKE3-derive("tsaur volume set v1", archive_b3 ‖ archive size ‖ piece size ‖ N ‖ M)` | which archive and which geometry; *not* the piece hashes, *not* the archive name |
| **descriptor hash** `descriptor_b3` | BLAKE3 of the CBOR descriptor (stored in every volume's trailer; printed by `split`, `inspect` and `fetch`) | the complete descriptor: set id, archive name, size, archive hash, geometry, every data and parity piece hash, Merkle root |
| **piece hash** | BLAKE3 of one zero-padded piece, listed in the descriptor | the bytes of that piece |

None of these says anything about *who* holds or sends the bytes.

## 2. How the receiver obtains the expected identity

The expectation must reach the receiver through a channel the receiver trusts, **not** from the
peer it is about to fetch from. Practical channels:

* the `split --json` report printed on the machine that made the split (`set_id`,
  `descriptor_b3`), copied by hand, by a message the user trusts, or on a note kept with the
  original;
* `inspect --json` run on a copy of the volumes the user already trusts (a drive they wrote
  themselves);
* a signed message or a QR code produced by the owner of the archive.

A set id or descriptor hash *received from the same peer* (for example through `SETS`) is only
a label for asking; it authenticates nothing, because the peer chose both the label and the
bytes. `fetch` therefore refuses to run without `--set`, and `--set`/`--descriptor` values must
come from the user, never from the wire.

## 3. What each option validates

| Option | What is checked | What it proves | What it does not prove |
|---|---|---|---|
| `--set <id>` (required) | the received descriptor parses and passes `validate()`; the set id recomputed from its `archive_b3`, size, piece size, N, M equals the expected id; the descriptor's own `set_id` field equals it | the descriptor describes *the archive with that hash* in *that geometry* | that the piece hashes are the right ones (they are not covered by the set id); who the peer is |
| `--descriptor <hash>` (recommended) | the received CBOR bytes hash to the expected value (checked *before* parsing) | the entire descriptor is the one the split produced, every piece hash included | who the peer is; that the peer will serve anything |
| neither | — | — | `fetch` refuses: "trust whatever arrives" is not a mode |

With `--set` only, a peer can hand over a descriptor with the right set id and wrong piece
hashes. Pieces matching those wrong hashes are then stored. The receiver is still protected, but
later and at a cost: `join` hashes the rebuilt archive and refuses to give it a name unless it
equals `archive_b3`, which *is* bound to the set id. Until a join succeeds, everything fetched in
set-id-only mode is provisional; the worst case is wasted transfer and disk space, never a wrong
archive. The `fetch` report says which of the two checks was decisive (`verified_against`), and
the CLI prints the provisional-until-join note in set-id-only mode.

**When the archive-hash check becomes decisive:** at `join`, always, for every mode, before
the output gets its final name. It is the only check that can catch a wrong descriptor in
set-id-only mode, and it re-validates everything in descriptor mode as well.

If the receiver already holds any volume of the set locally, the descriptor from that local
volume is the reference; a received descriptor is only compared with it, never adopted.

## 4. What is verified, and when

| Step | Check | On failure |
|---|---|---|
| descriptor received | size below `MAX_DESCRIPTOR` before it is read; `--descriptor` hash when given; CBOR parses; `validate()` (geometry, counts, piece limit, hash lengths, name length and characters); recomputed set id | descriptor discarded, next peer tried, nothing written |
| piece received | announced length equals the piece size (refused before allocation otherwise); BLAKE3 equals the descriptor entry for that (volume, stripe) | piece discarded and counted as rejected, next peer tried; nothing written |
| resume of a partial volume | the partial file's header names this set, index and piece size; **every piece the map declares complete is read back and hash-checked**; a missing or damaged map means every position is checked; a short file is extended and its missing tail treated as absent | pieces that fail are fetched again; a partial file with a foreign header is deleted and started over |
| volume completed | pieces, descriptor and trailer written; renamed from `.partial` only when every piece has verified | partial file and map stay for the next resume |
| join | every piece hash again, then the whole archive against `archive_b3` before the final rename | temporary output removed, exit code 5 (missing) or 2 (corrupt) |

Sender claims are never trusted: the receiver asks for a specific (set id, volume, stripe) and
verifies the answer; a `HAVE` list only decides which peer to ask first. A peer can lie only by
sending garbage, which costs it bandwidth and costs the receiver one retry per lie.

The local file names of fetched volumes are derived from the descriptor's archive name only
when that name is a safe single path component (no separators, colons, control characters,
reserved device names, trailing dots or spaces); otherwise the set id names the files. Peers
cannot choose where the receiver writes.

## 5. What a hash alone does not authenticate

* **Authorship or intent.** A matching hash proves the bytes are the expected bytes, not that a
  particular person produced them or that the archive's contents are what someone claims. Who
  made the archive is the job of the Ed25519 signature *inside* the `.tsr`, verified with a
  public key the user already trusts; volumes and transfers add no authorship claim.
* **The peer's identity.** Nothing in `TSXP/1` itself proves who answered. A hash from the
  descriptor cannot double as a proof of the peer, because the peer sent both. Only the TLS mode
  (§6) authenticates the peer, and only against a fingerprint the user pinned beforehand.
* **Confidentiality.** In plain mode descriptors and pieces travel in clear and are readable by
  every host on the path. An encrypted archive stays encrypted inside its volumes (piece hashes
  cover ciphertext); an unencrypted archive is public to anyone who can reach the port. Split
  before, not instead of, encrypting; use the TLS mode when the network is shared.
* **Metadata privacy.** The descriptor discloses the archive file name, its size, its hash and
  every piece hash; a serving peer learns which pieces are requested and by which address.
* **Freshness.** Descriptors are immutable and content-derived; there is no "newer version" a
  hash could protect against being rolled back.
* **Availability.** A correct hash cannot recover data nobody holds; a peer can refuse, stall
  (bounded by the 30 s timeouts), run out of connections, or throttle every client behind its
  bandwidth cap.

## 6. Two transports: plain TCP, and TLS with locally pinned identities

`TSXP/1` runs either over plain TCP or inside TLS 1.3. The requests, the replies and every check
of §3–§4 are identical in both; the TLS mode adds confidentiality and peer authentication, and
nothing else changes.

### 6.1 Plain mode (default)

On a shared network anyone can read the pieces, impersonate a peer or inject garbage (which the
hashes reject). `serve` therefore listens on loopback by default and requires `--expose-lan` for
anything else, with a printed warning. Integrity yes, secrecy and peer identity no.

### 6.2 TLS mode with pinned fingerprints

Each instance creates an identity once: `tsaur volumes keygen --out peer.key` writes an ECDSA
P-256 private key (`peer.key`, PEM, never leaves the machine) and a self-signed certificate
(`peer.key.crt`) and prints the certificate's **fingerprint** (SHA-256 of the DER certificate,
64 hex characters). Nothing in the certificate is trusted except that fingerprint: names,
validity dates and issuer are ignored on purpose, because there is no authority to vouch for
them. The TLS stack is `rustls` (ring provider); T-saur adds only the two pinning verifiers.

Three separate properties decide what a client on the network gets, and `serve` never
implies any of them: **encryption** (is the path readable by others?), **server identity** (can
the client verify whom it talks to?) and **client authorization** (who may fetch at all?).

| `serve` form (a non-loopback `--listen` always needs `--expose-lan`) | Encryption | Server identity | Client authorization |
|---|---|---|---|
| `serve` on loopback (default) | none | none | processes on this machine only |
| `serve --expose-lan --allow-anyone` | **none** | **none** | **anyone** who can reach the port |
| `serve --expose-lan --tls-identity s.key --allow-anyone` | TLS 1.3 | yes: clients pin its fingerprint with `--peer-id` | **anyone** who can reach the port |
| `serve --expose-lan --tls-identity s.key --allow <fp> ...` | TLS 1.3 | yes | the listed client fingerprints only (mutual TLS) |
| `serve --expose-lan --tls-identity s.key --allow-set <set id>=<fp> ...` (also `--allow-file`) | TLS 1.3 | yes | listed fingerprints, each for its own sets (mutual TLS); a set outside a client's lists is answered like an unknown set (§6.4) |

`--expose-lan` alone is refused: who may fetch has to be named, either with `--allow` (client
fingerprints, needs `--tls-identity`) or with `--allow-anyone`. Anonymous access is therefore
always an explicit choice; the server states the resulting access on stdout (`access:` line,
`access` object in `--json`) and warns on stderr when clients are anonymous. On the fetching
side, `fetch ... --from A:port --peer-id <A's fingerprint> [--tls-identity client.key]` pins A's
fingerprint (one `--peer-id` per `--from`, in the same order) and presents the client's own
identity when it has one; without `--peer-id` the connection is plain.

Rules that follow from the pinning:

* **Fingerprints travel through the trusted channel, like the set id** (§2). A fingerprint
  received from the same peer (for example printed by a `serve` you cannot see, or sent over
  the very connection it is meant to protect) authenticates nothing. The person running `serve`
  reads the fingerprint on that machine and hands it to the person running `fetch`; the client's
  fingerprint goes the other way for the allow list.
* **A mismatch ends the handshake before any request byte** is read or sent: a wrong `--peer-id`
  fails on the client with "does not match the pinned identity", a client outside the allow
  list is closed by the server without a reply, and nothing is written locally in either case.
* **What TLS adds and what it does not.** It adds secrecy on the path and the certainty that the
  bytes come from the holder of the pinned private key. It does *not* change what the bytes
  mean: the descriptor and piece checks of §3 stay decisive, because a pinned peer can still
  hold a wrong or damaged volume. It does not hide the fact that two addresses talk, nor the
  amount of data. It does not prove authorship of the archive (that is the Ed25519 signature
  inside the `.tsr`).
* **Keys are files.** Whoever can read `peer.key` can impersonate that peer; keep it with the
  same care as an SSH key. Losing it means creating a new identity and re-pinning it on the
  other side. Revocation is local because there is no authority: each side may pass
  `--revoke FILE`, a text file of fingerprints it refuses even when they are pinned or allowed
  (§6.3). A revocation reaches the other side through the same trusted channel as a
  fingerprint; nothing on the wire can add or remove one, and the file is read once at start-up.
* **Plain and TLS do not mix.** A plain `fetch` against a TLS `serve`, or the reverse, fails at
  once (the plain side sees non-ASCII bytes or a closed connection, the TLS side sees an invalid
  record) without a fallback in either direction, so a misconfiguration cannot silently
  downgrade to clear text.
* **Exposure stays explicit.** `--expose-lan` is required for a non-loopback address in both
  modes, and it must be paired with `--allow` or `--allow-anyone` (table above).
* **Limits apply to every client, pinned or not.** A pinned identity says who a client is, not
  that it behaves: the per-address request rate, the connection counts, the cap on distinct
  source addresses, the time budget of each connection and the global bandwidth cap
  (`VOLUME-SETS.md` §8.2) apply in both modes. In TLS mode a connection above the counts is
  closed before the handshake, and the client retries with a bounded back-off. The bandwidth
  cap is shared by all connections and the time a connection spends throttled is credited to
  its budget, so the cap can never make a well-behaved client look too slow. A source address
  is a resource key, not an identity: several clients behind one NAT share one address slot.

### 6.3 Revocation

`--revoke FILE` names a text file with one certificate fingerprint per line (64 hex characters;
colons and letter case tolerated; `#` starts a comment; blank lines ignored; at most 1 MiB, so a
wrong path fails fast). `serve` and `fetch` read it once at start-up (restart to apply a change)
and refuse every fingerprint in it:

* **Before the handshake.** `serve` removes revoked identities from its effective allow lists
  and reports how many (`revoked K` on the `access:` line, `"revoked"` in `--json`); when nothing
  would be left it refuses to start (exit 3, "every allowed identity is revoked"), because a
  server nobody can use hides a mistake. `fetch` refuses the whole command (exit 3) when any
  `--peer-id` is revoked, naming the file: skipping the peer silently would make a typo in the
  wrong file look like an unreachable peer.
* **At the handshake.** The verifiers check the revocation list before the pin or the allow
  list, so a revoked client is closed by the server without a reply and a revoked server fails
  on the client with "is revoked", before any request byte, whatever the lists say.
* **What it does not do.** No propagation, no expiry, no effect on `--allow-anyone` (anonymous
  clients present no certificate, so `serve --revoke` requires an allow list; `fetch --revoke`
  requires `--peer-id`, and `serve --revoke` requires `--tls-identity`). A server whose own
  fingerprint is in the file starts with a warning; clients decide what they pin.

### 6.4 Which client may read which set

`--allow <fp>` admits a client to every served set. `--allow-set <set id>=<fp>` (repeatable;
the set id may be a prefix of at least 16 hex characters that matches exactly one served set,
resolved before anything is bound) and `--allow-file FILE` (one rule per line, `<fingerprint>
[set id ...]`, no set id = every set) admit a client to specific sets only. The handshake admits
the union of all lists (minus revocations); after it, the server reads the client's verified
certificate and consults the lists for every request: `SETS` lists only the sets the client may
read, and `DESCRIPTOR`, `HAVE` and `PIECE` for another set are answered exactly like an unknown
set (`ERR 404 unknown set`), so a client learns nothing about sets it may not read, not even
that they exist. The `404` was chosen over a silent close on purpose: a close is what the client
treats as "busy" and retries sixteen times, which would turn an authorization failure into a
slow one. Per-set lists need mutual TLS (`--tls-identity`) and exclude `--allow-anyone`; the
start-up listing shows `readers: all | N` per set so an operator can check the mapping without
exposing fingerprints.

What is still missing after this step: more than two participants per fetch, measurements on
two real devices (`TWO-DEVICE-BENCHMARK-PLAN.md`), and an independent review of the whole
exchange path by another evaluator (`docs/review/REVIEW-PACKAGE.md`, `ROADMAP.md`).
