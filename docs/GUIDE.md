# T-saur user guide

Everything here works offline, without an account or a service. Commands are shown for a shell
(`bash`, Git Bash on Windows, or PowerShell with the obvious quoting changes); `tsaur` is the
binary from a release package or from `cargo build --release`. Every `bash` block that is not
marked *not run automatically* is executed by `tools/check_guide.py` against the packaged
binary before a release, in the order shown, starting from an empty directory.

Two builds exist (`docs/DISTRIBUTION-POLICY.md`): **lite** (the default download, no Lepton,
writes archives every build can read) and **full** (adds Lepton JPEG recompression; archives
that contain recompressed JPEGs need the full build to open). The commands are the same.

## 1. Install

1. Download the package for your platform (`tsaur-<version>-<platform>-lite.zip`; the full build is
   built from source, see `docs/DISTRIBUTION-POLICY.md`)
   and `SHA256SUMS`, then check the hash before unpacking:
   `sha256sum -c SHA256SUMS` (Linux/macOS) or `Get-FileHash tsaur-*.zip` (PowerShell) compared
   with the listed value.
2. Unpack it and put the `tsaur` binary on your `PATH` (or call it by path).
3. `tsaur --version` prints the version; `tsaur --help` lists the commands.

From source: `cd tsaur && cargo build --release` (Rust 1.87 or newer); add
`--no-default-features` for the lite build. The binary is `tsaur/target/release/tsaur`.

## 2. Your first archive

Create a few files to work with:

```bash
mkdir -p sample/docs
printf 'T-saur keeps every byte and verifies it on the way back.\n%.0s' $(seq 1 400) > sample/docs/notes.txt
cp sample/docs/notes.txt sample/docs/notes-v2.txt
printf 'second version: one more line\n' >> sample/docs/notes-v2.txt
head -c 100000 /dev/urandom > sample/photo.bin 2>/dev/null || python3 -c "import os;open('sample/photo.bin','wb').write(os.urandom(100000))"
tsaur --version
```

Pack, look inside, verify, restore:

```bash
tsaur pack demo.tsr sample                      # deterministic: same inputs and options, same bytes
tsaur list demo.tsr                              # entries, sizes, hashes
tsaur info demo.tsr                              # sections, codecs, chunks, what the reader needs
tsaur verify demo.tsr                            # every entry decoded and hash-checked; exit 0 = intact
tsaur unpack demo.tsr restored/                  # writes each file under a temporary name, renames after its hash verified
cmp sample/docs/notes.txt restored/docs/notes.txt && echo "bit-exact"
```

Rules you can rely on: `unpack` refuses to replace files that already exist unless you pass
`--overwrite`; entry paths can never leave the destination directory (absolute paths, `..`,
drive letters and Windows device names are rejected when the archive is read); symbolic links
inside the inputs are skipped and never recreated.

```bash
set +e; tsaur unpack demo.tsr restored/; code=$?; set -e     # exit 3 = refused by policy, nothing written
test "$code" -eq 3 && echo "refused: files exist (exit 3)"
tsaur unpack demo.tsr restored/ --overwrite
```

Options worth knowing: `--no-lepton` (full build only: store JPEGs as they are, so the lite build
can open the archive), `--solid 8` (larger blocks, best ratio), `--codec zstd --level 9` (fast),
`--canonical` (also store Markdown/text views of DOCX and PDF files for agents), `--ref old.tsr`
(store only what `old.tsr` does not already hold), `--pieces` (transport pieces + parity sidecars).

## 3. Encryption and signatures (what is protected, and what is not)

```bash
tsaur keygen --out signing.key                   # Ed25519 signing key (hex seed); signing.key.pub is the public key
tsaur pack secret.tsr sample --password "correct horse" --sign-key signing.key
tsaur info secret.tsr                            # works without the password: shows how the archive can be unlocked
tsaur verify secret.tsr --password "correct horse" --pubkey "$(cat signing.key.pub)"
tsaur unpack secret.tsr restored-secret/ --password "correct horse"
```

* `--password` and/or `--to recipient.pub` (hybrid X25519 + ML-KEM-768 recipients from
  `keygen --recipient`) encrypt the **content and the manifest** of the archive: every blob is
  sealed with XChaCha20-Poly1305 and the archive key is wrapped for each password or recipient.
  File names, sizes and hashes are inside the encrypted manifest.
* Without those options **nothing in the archive is encrypted**: anyone holding the file, its
  volumes or its pieces can read everything. Volumes and transfers carry the archive bytes as they
  are; the TLS mode of §5 protects only the connection.
* A signature proves who made the archive only to someone who already trusts the public key;
  `verify --pubkey` is the check, the archive's own claim is never trusted.

## 4. Volumes: spread an archive over several drives, lose one, restore

`split` cuts the archive into N data volumes plus M parity volumes (`.tsrv` files); any N of the
N + M volumes rebuild it bit-exact. Here: 3 data + 1 parity into four directories that stand
for four drives.

```bash
mkdir -p drive1 drive2 drive3 drive4                              # output locations must exist: a drive that is not mounted is an error, never a folder on the system disk
tsaur volumes split demo.tsr --data 3 --parity 1 --out drive1 --out drive2 --out drive3 --out drive4
tsaur volumes inspect drive1 drive2 drive3 drive4 --verify        # every piece hash-checked; "reconstructible: yes"
```

Now lose a drive:

```bash
rm -r drive2
tsaur volumes inspect drive1 drive3 drive4 --verify               # one volume missing, still reconstructible
tsaur volumes join demo-rebuilt.tsr drive1 drive3 drive4           # written to a temporary name, renamed after the archive hash matched
cmp demo.tsr demo-rebuilt.tsr && echo "identical to the original archive"
tsaur unpack demo-rebuilt.tsr restored-from-volumes/
cmp sample/photo.bin restored-from-volumes/photo.bin && echo "files bit-exact"
```

Recreate the lost volume so that the set is complete again (byte-identical to the one that was
lost):

```bash
mkdir drive2-new
tsaur volumes repair drive1 drive3 drive4 --out drive2-new
tsaur volumes inspect drive1 drive2-new drive3 drive4 --verify
```

Losing more volumes than the parity count (here two) makes the archive unrecoverable from what
is left; `inspect` says so ("reconstructible: NO", exit 5) and `join` refuses instead of writing
a wrong file. Choose the parity count by how many drives you can afford to lose.

## 5. Moving volumes to another machine over the network

Two instances can exchange the pieces of a set directly, with the addresses you supply. Nothing
is discovered automatically and no service is involved. Each side creates an identity once and
hands its **fingerprint** to the other side through a channel it trusts (a message, a note, a
phone call), together with the set id and descriptor hash printed by `split`:

```bash
tsaur volumes keygen --out peer.key                               # prints the fingerprint the other side pins
tsaur volumes fingerprint peer.key
```

On the machine that has the volumes (*not run automatically*: it serves until interrupted):

```bash
tsaur volumes serve drive1 drive3 drive4 --listen 192.168.1.10:7407 --expose-lan --tls-identity peer.key --allow <fingerprint of the receiver>
```

On the receiver (*not run automatically*: it needs the values from the other machine):

```bash
tsaur volumes fetch --set <set id> --descriptor <descriptor hash> --from 192.168.1.10:7407 --peer-id <fingerprint of the server> --tls-identity peer.key --out ./here --join demo.tsr
```

The receiver checks the descriptor against the set id and descriptor hash it already knows and
every piece against the descriptor before writing it; interrupted fetches resume and pieces
already on disk are hash-checked again. `serve` listens on loopback unless `--expose-lan` is
given together with who may fetch: `--allow <fingerprint>` (encrypted, both sides
authenticated) or the explicit `--allow-anyone`. Without `--tls-identity` the connection is
plain TCP: fine on a machine you own, readable by everyone on a shared network. Details:
`docs/design/VOLUME-TRUST.md`.

## 6. Reading without extracting (for people and for agents)

```bash
tsaur stat demo.tsr docs/notes.txt               # size, hash, citation URI
tsaur grep demo.tsr "verifies" --entries "*.txt" # search inside entries
tsaur read demo.tsr docs/notes.txt --lines 1-3   # a range of lines, hash-verified
tsaur list demo.tsr --md                         # Markdown view with token estimates for an agent
tsaur diff demo.tsr demo-rebuilt.tsr             # entries added / removed / changed between two archives
```

`tsaur mcp --root <dir>` serves the same operations over the Model Context Protocol on
stdin/stdout (tools `tsaur_list`, `tsaur_read`, `tsaur_grep`, `tsaur_verify`, `tsaur_unpack`, ...);
every result is hash-verified and labelled as data for the model, never as instructions.

## 7. Exit codes

| Code | Meaning | Typical cause |
|---:|---|---|
| 0 | success | |
| 1 | I/O error | missing file, permission, disk full |
| 2 | corrupt or integrity failure | damaged archive, wrong hash, unsupported format version |
| 3 | refused by policy | unsafe path, existing file without `--overwrite`, network exposure without `--allow`/`--allow-anyone` |
| 4 | resource limit | declared sizes above the reader ceilings |
| 5 | missing data | lost volumes beyond parity, reference archive not given, peer without the pieces |
| 6 | cryptography | wrong password, signature mismatch, TLS identity mismatch |
| 7 | invalid argument or unsupported feature | bad option, archive needs the full build (`requires: lepton`) |

Clean up the sandbox of this guide:

```bash
rm -rf sample restored restored-secret restored-from-volumes drive1 drive2 drive2-new drive3 drive4 here demo.tsr demo-rebuilt.tsr secret.tsr signing.key signing.key.pub peer.key peer.key.crt
```
