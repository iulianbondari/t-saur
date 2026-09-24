"""The .tsrp prototype format (v0) - pack / unpack / verify / recover.

Main file layout (self-contained for local use):
    "TSRP" | u8 version | u8 flags | u16 reserved | u32 manifest_len | manifest (CBOR, encrypted if flags&1)
    | body (blobs of chunks or solid blocks, compressed, optionally encrypted)

Sidecars (optional, for P2P + recovery, like .torrent + PAR2):
    <archive>.meta  - CBOR: piece size, hash per piece (BLAKE2b-256), Merkle root, RS parameters
    <archive>.par   - Reed-Solomon parity blocks (any k of the k+m pieces rebuild the file)

Manifest (CBOR):
    { "v":0, "created":..., "chunking":{...}, "dict":{"size":..,"hash":..,"off":..,"len":..} | None,
      "codecs":{id:name}, "blobs":[ {"h":hash,"off":..,"clen":..,"ulen":..,"c":codec_id,"n":[chunk hashes] } ],
      "entries":[ {"path":..,"size":..,"h":hash,"mode":"raw|zip|canonical-md","chunks":[hash...] | "members":{...}} ],
      "crypto": {"kdf":"argon2id", "salt":.., "ops":.., "mem":.., "wrapped_dek":..} | None }

Not the final format. It serves to measure the ideas (dedup, dictionary, solid blocks, container-aware,
canonical mode, per-chunk encryption, pieces + parity).
"""
from __future__ import annotations

import hashlib
import io
import lzma
import os
import struct
import time
from dataclasses import dataclass, field
from pathlib import Path

import cbor2
import zstandard as zstd

from . import cdc, containers, rs

MAGIC = b"TSRP"
VERSION = 0
FLAG_ENCRYPTED = 1

CODEC_STORE = 0
CODEC_ZSTD_DICT = 1
CODEC_ZSTD = 2
CODEC_XZ = 3
CODEC_NAMES = {CODEC_STORE: "store", CODEC_ZSTD_DICT: "zstd19+dict", CODEC_ZSTD: "zstd22", CODEC_XZ: "xz9e"}


def h256(data: bytes) -> bytes:
    """BLAKE2b-256 - stand-in for BLAKE3 (which will be used in the Rust implementation)."""
    return hashlib.blake2b(data, digest_size=32).digest()


def merkle_root(hashes: list[bytes]) -> bytes:
    if not hashes:
        return h256(b"")
    level = [h256(b"\x00" + h) for h in hashes]
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        level = [h256(b"\x01" + level[i] + level[i + 1]) for i in range(0, len(level), 2)]
    return level[0]


# ----------------------------------------------------------------------------- crypto
class Crypto:
    """Envelope encryption: random DEK (32B) wrapped with KEK = Argon2id(password). AEAD XChaCha20-Poly1305 per blob."""

    def __init__(self, dek: bytes):
        from nacl.secret import Aead
        self.dek = dek
        self.aead = Aead(dek)
        self.nonce_key = h256(b"aix-nonce" + dek)

    def nonce(self, label: bytes, index: int) -> bytes:
        return hashlib.blake2b(label + struct.pack(">Q", index), key=self.nonce_key, digest_size=24).digest()

    def seal(self, label: bytes, index: int, data: bytes, aad: bytes = b"") -> bytes:
        return self.aead.encrypt(data, aad, self.nonce(label, index)).ciphertext

    def open(self, label: bytes, index: int, data: bytes, aad: bytes = b"") -> bytes:
        return self.aead.decrypt(data, aad, self.nonce(label, index))

    @staticmethod
    def kdf(password: str, salt: bytes, ops: int, mem: int) -> bytes:
        from nacl.pwhash import argon2id
        return argon2id.kdf(32, password.encode("utf-8"), salt, opslimit=ops, memlimit=mem)

    @classmethod
    def new_from_password(cls, password: str) -> tuple["Crypto", dict]:
        from nacl.pwhash import argon2id
        from nacl.secret import Aead
        salt = os.urandom(argon2id.SALTBYTES)
        ops, mem = argon2id.OPSLIMIT_MODERATE, argon2id.MEMLIMIT_MODERATE
        kek = cls.kdf(password, salt, ops, mem)
        dek = os.urandom(32)
        wrapped = Aead(kek).encrypt(dek, b"aix-dek", os.urandom(Aead.NONCE_SIZE))  # nonce included
        return cls(dek), {"kdf": "argon2id", "salt": salt, "ops": ops, "mem": mem, "wrapped_dek": bytes(wrapped), "aead": "xchacha20-poly1305"}

    @classmethod
    def from_password(cls, password: str, params: dict) -> "Crypto":
        from nacl.secret import Aead
        kek = cls.kdf(password, params["salt"], params["ops"], params["mem"])
        w = params["wrapped_dek"]
        dek = Aead(kek).decrypt(w[Aead.NONCE_SIZE:], b"aix-dek", w[:Aead.NONCE_SIZE])
        return cls(dek)


# ----------------------------------------------------------------------------- options
@dataclass
class PackOptions:
    chunk_min: int = 2048
    chunk_avg: int = 8192
    chunk_max: int = 65536
    solid_block: int = 0            # 0 = per-chunk compression; >0 = solid blocks of at most N bytes
    train_dict: bool = True         # trained zstd dictionary (useful ONLY in per-chunk mode; in solid mode it is overhead)
    dict_in_solid: bool = False     # force the dictionary in solid mode too (for experiments)
    dict_size: int = 110 * 1024
    container_aware: bool = True    # DOCX exploded (verified bit-exact)
    canonical: bool = False         # agent mode: DOCX/PDF -> markdown (NOT bit-exact)
    codecs: tuple[int, ...] = (CODEC_ZSTD_DICT, CODEC_ZSTD, CODEC_XZ)
    password: str | None = None
    piece_size: int = 64 * 1024     # for the .meta / .par sidecars
    parity_ratio: float = 0.10      # 10% RS parity pieces
    stats: dict = field(default_factory=dict)


# ----------------------------------------------------------------------------- pack
class Packer:
    def __init__(self, opts: PackOptions):
        self.o = opts
        self.chunks: dict[bytes, bytes] = {}      # hash -> plaintext (unique)
        self.order: list[bytes] = []              # order of first appearance (the index in the chunk table)
        self.index: dict[bytes, int] = {}         # hash -> index in the table
        self.entries: list[dict] = []
        self.input_bytes = 0                      # sum of the original files
        self.logical_bytes = 0                    # after container expansion (docx -> XML etc.)
        self.chunk_refs = 0

    # --- chunking + dedup
    def _add_stream(self, data: bytes) -> list[int]:
        """Returns the list of chunk INDICES (not hashes) - compact manifest."""
        refs = []
        for c in cdc.chunk(data, min_size=self.o.chunk_min, avg_size=self.o.chunk_avg, max_size=self.o.chunk_max):
            h = h256(c)
            if h not in self.chunks:
                self.chunks[h] = c
                self.index[h] = len(self.order)
                self.order.append(h)
            refs.append(self.index[h])
            self.chunk_refs += 1
        self.logical_bytes += len(data)
        return refs

    def add_file(self, path: Path, arcname: str | None = None):
        data = path.read_bytes()
        self.input_bytes += len(data)
        name = arcname or path.name
        ext = path.suffix.lower()
        entry = {"path": name, "size": len(data), "h": h256(data), "mtime": int(path.stat().st_mtime)}

        if self.o.canonical and ext in (".docx", ".pdf"):
            md = containers.docx_to_markdown(data) if ext == ".docx" else containers.pdf_to_markdown(data)
            md_b = md.encode("utf-8")
            entry.update({"mode": "canonical-md", "path": name + ".md", "canon_size": len(md_b),
                          "chunks": self._add_stream(md_b), "note": "semantically derived, not bit-exact"})
            self.entries.append(entry)
            return

        if self.o.container_aware and ext == ".docx":
            res = containers.try_bitexact_zip(data)
            if res is not None:
                recipe, parts, level = res
                members = {}
                for m in recipe.members:
                    members[m["name"]] = self._add_stream(parts[m["name"]])
                entry.update({"mode": "zip", "level": level, "recipe": recipe.__dict__, "members": members})
                self.entries.append(entry)
                return
            entry["note"] = "zip: could not be rebuilt bit-exact -> stored raw"

        entry.update({"mode": "raw", "chunks": self._add_stream(data)})
        self.entries.append(entry)

    # --- compression
    def _train_dict(self) -> zstd.ZstdCompressionDict | None:
        if not self.o.train_dict or len(self.chunks) < 8:
            return None
        if self.o.solid_block > 0 and not self.o.dict_in_solid:
            return None  # in solid mode the context comes from the block; the dictionary would only be overhead
        samples = [self.chunks[h] for h in self.order]
        try:
            return zstd.train_dictionary(self.o.dict_size, samples)
        except zstd.ZstdError:
            return None

    def _compress_best(self, data: bytes, zdict) -> tuple[int, bytes]:
        best = (CODEC_STORE, data)
        for codec in self.o.codecs:
            if codec == CODEC_ZSTD_DICT:
                if zdict is None:
                    continue
                out = zstd.ZstdCompressor(level=19, dict_data=zdict).compress(data)
            elif codec == CODEC_ZSTD:
                params = zstd.ZstdCompressionParameters.from_level(22, window_log=27, enable_ldm=True)
                out = zstd.ZstdCompressor(compression_params=params).compress(data)
            elif codec == CODEC_XZ:
                out = lzma.compress(data, format=lzma.FORMAT_XZ, preset=9 | lzma.PRESET_EXTREME)
            else:
                continue
            if len(out) < len(best[1]):
                best = (codec, out)
        return best

    def finish(self, out_path: Path) -> dict:
        t0 = time.perf_counter()
        crypto, crypto_params = (None, None)
        if self.o.password:
            crypto, crypto_params = Crypto.new_from_password(self.o.password)

        zdict = self._train_dict()
        body = io.BytesIO()
        blobs = []
        dict_info = None
        if zdict is not None:
            raw = zdict.as_bytes()
            enc = crypto.seal(b"dict", 0, raw) if crypto else raw
            dict_info = {"size": len(raw), "hash": h256(raw), "off": body.tell(), "len": len(enc), "id": zdict.dict_id()}
            body.write(enc)

        # grouping of the unique chunks into blobs: per chunk or solid blocks (indices in the chunk table)
        groups: list[list[int]] = []
        if self.o.solid_block > 0:
            cur, cur_len = [], 0
            for idx, h in enumerate(self.order):
                c = self.chunks[h]
                if cur and cur_len + len(c) > self.o.solid_block:
                    groups.append(cur)
                    cur, cur_len = [], 0
                cur.append(idx)
                cur_len += len(c)
            if cur:
                groups.append(cur)
        else:
            groups = [[idx] for idx in range(len(self.order))]

        codec_hist: dict[str, int] = {}
        for i, g in enumerate(groups):
            plain = b"".join(self.chunks[self.order[idx]] for idx in g)
            codec, comp = self._compress_best(plain, zdict)
            codec_hist[CODEC_NAMES[codec]] = codec_hist.get(CODEC_NAMES[codec], 0) + 1
            enc = crypto.seal(b"blob", i, comp, aad=struct.pack(">I", codec)) if crypto else comp
            blobs.append({"off": body.tell(), "clen": len(enc), "ulen": len(plain), "c": codec, "first": g[0], "count": len(g)})
            body.write(enc)

        # chunk table: hash (32B) + size, once per unique chunk
        chunk_table = [[h, len(self.chunks[h])] for h in self.order]
        manifest = {
            "v": VERSION, "created": int(time.time()),
            "chunking": {"algo": "fastcdc-gear64", "min": self.o.chunk_min, "avg": self.o.chunk_avg, "max": self.o.chunk_max},
            "solid_block": self.o.solid_block, "dict": dict_info, "codecs": CODEC_NAMES,
            "chunks": chunk_table, "blobs": blobs, "entries": self.entries, "crypto": crypto_params,
            "hash": "blake2b-256", "canonical": self.o.canonical,
        }
        man = zstd.ZstdCompressor(level=19).compress(cbor2.dumps(manifest))  # the manifest itself is compressed
        man_enc = crypto.seal(b"manifest", 0, man) if crypto else man
        flags = FLAG_ENCRYPTED if crypto else 0
        header = MAGIC + struct.pack(">BBHI", VERSION, flags, 0, len(man_enc))
        if crypto:
            # the KDF parameters must be in the clear so that the key can be derived
            pre = cbor2.dumps(crypto_params)
            header += struct.pack(">I", len(pre)) + pre
        else:
            header += struct.pack(">I", 0)
        out_path.write_bytes(header + man_enc + body.getvalue())

        unique_bytes = sum(len(c) for c in self.chunks.values())
        stats = {
            "archive_bytes": out_path.stat().st_size, "input_bytes": self.input_bytes, "logical_bytes": self.logical_bytes,
            "unique_chunk_bytes": unique_bytes, "chunks_total": self.chunk_refs, "chunks_unique": len(self.chunks),
            "dedup_saved_bytes": self.logical_bytes - unique_bytes, "manifest_bytes": len(man_enc),
            "dict_bytes": dict_info["len"] if dict_info else 0, "blobs": len(blobs), "codec_hist": codec_hist,
            "pack_seconds": round(time.perf_counter() - t0, 3), "encrypted": bool(crypto),
        }
        self.o.stats = stats
        return stats


def pack(files: list[Path], out_path: Path, opts: PackOptions | None = None) -> dict:
    opts = opts or PackOptions()
    p = Packer(opts)
    for f in files:
        p.add_file(f)
    return p.finish(out_path)


# ----------------------------------------------------------------------------- unpack
class Reader:
    def __init__(self, path: Path, password: str | None = None):
        self.raw = path.read_bytes()
        if self.raw[:4] != MAGIC:
            raise ValueError("not a TSRP archive")
        ver, flags, _res, man_len = struct.unpack(">BBHI", self.raw[4:12])
        pre_len = struct.unpack(">I", self.raw[12:16])[0]
        pos = 16
        self.crypto = None
        if flags & FLAG_ENCRYPTED:
            if password is None:
                raise ValueError("the archive is encrypted: password missing")
            params = cbor2.loads(self.raw[pos:pos + pre_len])
            self.crypto = Crypto.from_password(password, params)
        pos += pre_len
        man = self.raw[pos:pos + man_len]
        pos += man_len
        if self.crypto:
            man = self.crypto.open(b"manifest", 0, man)
        self.manifest = cbor2.loads(zstd.ZstdDecompressor().decompress(man, max_output_size=1 << 30))
        self.body_off = pos
        self.table = self.manifest["chunks"]  # [[hash, size], ...]
        self.zdict = None
        d = self.manifest.get("dict")
        if d:
            raw = self.raw[self.body_off + d["off"]: self.body_off + d["off"] + d["len"]]
            if self.crypto:
                raw = self.crypto.open(b"dict", 0, raw)
            if h256(raw) != d["hash"]:
                raise ValueError("corrupt dictionary")
            self.zdict = zstd.ZstdCompressionDict(raw)
        self._chunk_cache: dict[int, bytes] = {}
        self._blob_by_chunk: dict[int, int] = {}
        for i, b in enumerate(self.manifest["blobs"]):
            for idx in range(b["first"], b["first"] + b["count"]):
                self._blob_by_chunk[idx] = i

    def _load_blob(self, i: int):
        b = self.manifest["blobs"][i]
        data = self.raw[self.body_off + b["off"]: self.body_off + b["off"] + b["clen"]]
        if self.crypto:
            data = self.crypto.open(b"blob", i, data, aad=struct.pack(">I", b["c"]))
        c = b["c"]
        if c == CODEC_STORE:
            plain = data
        elif c == CODEC_ZSTD_DICT:
            plain = zstd.ZstdDecompressor(dict_data=self.zdict).decompress(data, max_output_size=b["ulen"])
        elif c == CODEC_ZSTD:
            plain = zstd.ZstdDecompressor(max_window_size=1 << 27).decompress(data, max_output_size=b["ulen"])
        elif c == CODEC_XZ:
            plain = lzma.decompress(data)
        else:
            raise ValueError(f"unknown codec {c}")
        if len(plain) != b["ulen"]:
            raise ValueError("wrong blob length")
        off = 0
        for idx in range(b["first"], b["first"] + b["count"]):
            h, sz = self.table[idx]
            piece = plain[off:off + sz]
            off += sz
            if h256(piece) != h:
                raise ValueError("corrupt chunk (hash mismatch)")
            self._chunk_cache[idx] = piece

    def chunk(self, idx: int) -> bytes:
        if idx not in self._chunk_cache:
            self._load_blob(self._blob_by_chunk[idx])
        return self._chunk_cache[idx]

    def stream(self, refs: list[int]) -> bytes:
        return b"".join(self.chunk(i) for i in refs)

    def extract_entry(self, e: dict) -> bytes:
        mode = e["mode"]
        if mode in ("raw", "canonical-md"):
            data = self.stream(e["chunks"])
        elif mode == "zip":
            recipe = containers.ZipRecipe(**e["recipe"])
            parts = {name: self.stream(refs) for name, refs in e["members"].items()}
            lvl = e["level"] if e["level"] >= 0 else None
            data = containers.rebuild_zip(recipe, parts, lvl)
        else:
            raise ValueError(f"unknown mode {mode}")
        if mode != "canonical-md" and h256(data) != e["h"]:
            raise ValueError(f"hash mismatch at {e['path']}")
        return data

    def extract_all(self, out_dir: Path) -> list[Path]:
        out_dir.mkdir(parents=True, exist_ok=True)
        written = []
        for e in self.manifest["entries"]:
            dest = out_dir / Path(e["path"]).name  # no path traversal: only the file name
            dest.write_bytes(self.extract_entry(e))
            written.append(dest)
        return written


# ----------------------------------------------------------------------------- pieces / parity (sidecars)
def write_sidecars(archive: Path, piece_size: int = 64 * 1024, parity_ratio: float = 0.10) -> dict:
    data = archive.read_bytes()
    pieces = [data[i:i + piece_size] for i in range(0, len(data), piece_size)]
    if pieces and len(pieces[-1]) < piece_size:
        pieces[-1] = pieces[-1] + b"\x00" * (piece_size - len(pieces[-1]))  # padding for RS
    hashes = [h256(p) for p in pieces]
    k = len(pieces)
    m = max(1, int(round(k * parity_ratio)))
    if k + m > 255:
        raise ValueError("prototype: max 255 pieces per stripe (in production: multiple stripes / RaptorQ)")
    parity = rs.encode(pieces, m) if k > 0 else []
    meta = {"file": archive.name, "size": len(data), "piece_size": piece_size, "k": k, "m": m,
            "pieces": hashes, "parity": [h256(p) for p in parity], "root": merkle_root(hashes),
            "rs": "gf256-vandermonde-systematic"}
    (archive.with_suffix(archive.suffix + ".meta")).write_bytes(cbor2.dumps(meta))
    (archive.with_suffix(archive.suffix + ".par")).write_bytes(b"".join(parity))
    return meta


def verify_pieces(archive: Path) -> tuple[list[int], dict]:
    meta = cbor2.loads(archive.with_suffix(archive.suffix + ".meta").read_bytes())
    data = archive.read_bytes()
    ps = meta["piece_size"]
    bad = []
    for i, h in enumerate(meta["pieces"]):
        p = data[i * ps:(i + 1) * ps]
        if len(p) < ps:
            p = p + b"\x00" * (ps - len(p))
        if h256(p) != h:
            bad.append(i)
    return bad, meta


def recover(archive: Path) -> tuple[int, bool]:
    """Rebuilds the corrupt pieces from the parity; returns (number of pieces repaired, ok)."""
    bad, meta = verify_pieces(archive)
    if not bad:
        return 0, True
    data = bytearray(archive.read_bytes())
    ps, k, m = meta["piece_size"], meta["k"], meta["m"]
    par = archive.with_suffix(archive.suffix + ".par").read_bytes()
    avail: dict[int, bytes] = {}
    for i in range(k):
        if i not in bad:
            p = bytes(data[i * ps:(i + 1) * ps])
            avail[i] = p + b"\x00" * (ps - len(p))
    for j in range(m):
        p = par[j * ps:(j + 1) * ps]
        if h256(p) == meta["parity"][j]:
            avail[k + j] = p
    if len(avail) < k:
        return 0, False
    fixed = rs.decode(avail, k, m)
    for i in bad:
        piece = fixed[i]
        end = min((i + 1) * ps, meta["size"])
        data[i * ps:end] = piece[: end - i * ps]
    archive.write_bytes(bytes(data))
    bad2, _ = verify_pieces(archive)
    return len(bad), not bad2
