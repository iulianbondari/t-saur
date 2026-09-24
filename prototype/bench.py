"""Benchmark: classic formats vs. the T-saur prototype on the corpus in benchmarks/.

Run:     python prototype/bench.py   (from the repository root)
Writes:  benchmarks/RESULTS.md  (+ the temporary archives in benchmarks/out/)

Classic methods measured locally with Python's libraries (xz -9e ~ LZMA2 from 7z); 7-Zip and WinRAR
are also run through their CLI when they are installed:
  zip(deflate9) per file | zip(bzip2) | zip(lzma) | tar+gzip9 | tar+bzip2 | tar+xz9e | tar+brotli11
  | tar+zstd19 | tar+zstd22 --long
"""
from __future__ import annotations

import bz2
import gzip
import hashlib
import io
import lzma
import os
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile
from pathlib import Path

import zstandard as zstd

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "prototype"))
from tsaurproto import archive as ax  # noqa: E402

BENCH = ROOT / "benchmarks"
OUT = BENCH / "out"


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def tar_bytes(files: list[Path]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:
        for f in sorted(files):
            t.add(f, arcname=f.name)
    return buf.getvalue()


def timed(fn):
    t0 = time.perf_counter()
    r = fn()
    return r, time.perf_counter() - t0


def brotli_cli(data: bytes) -> bytes | None:
    exe = shutil.which("brotli")
    if not exe:
        return None
    src = OUT / "_tmp.tar"
    dst = OUT / "_tmp.tar.br"
    src.write_bytes(data)
    if dst.exists():
        dst.unlink()
    r = subprocess.run([exe, "-q", "11", "-w", "24", "-f", "-o", str(dst), str(src)], capture_output=True)
    if r.returncode != 0 or not dst.exists():
        return None
    return dst.read_bytes()


SEVENZIP = Path(r"C:\Program Files\7-Zip\7z.exe")
RAR = Path(r"C:\Program Files\WinRAR\Rar.exe")


def external(files: list[Path], name: str, cmd: list[str], out: Path) -> dict:
    """Runs an external archiver (7z / rar) and measures the size + time."""
    if out.exists():
        out.unlink()
    t0 = time.perf_counter()
    r = subprocess.run(cmd + [str(f) for f in sorted(files)], capture_output=True, cwd=str(OUT))
    dt = time.perf_counter() - t0
    if r.returncode != 0 or not out.exists():
        return {"method": name, "bytes": None, "seconds": dt, "solid": True, "error": r.stderr.decode(errors="replace")[-200:]}
    return {"method": name, "bytes": out.stat().st_size, "seconds": dt, "solid": True}


def external_rows(files: list[Path]) -> list[dict]:
    rows = []
    if SEVENZIP.exists():
        z = str(SEVENZIP)
        rows.append(external(files, "7-Zip 26.03: 7z LZMA2 -mx9 solid", [z, "a", "-t7z", "-mx9", "-m0=lzma2", "-ms=on", "-mqs=on", "-bso0", "-bsp0", str(OUT / "x_lzma2.7z")], OUT / "x_lzma2.7z"))
        rows.append(external(files, "7-Zip 26.03: 7z PPMd -mx9 (o32, 1 GB) solid", [z, "a", "-t7z", "-mx9", "-m0=ppmd:mem=1g:o=32", "-ms=on", "-mqs=on", "-bso0", "-bsp0", str(OUT / "x_ppmd.7z")], OUT / "x_ppmd.7z"))
        rows.append(external(files, "7-Zip 26.03: zip Deflate -mx9", [z, "a", "-tzip", "-mx9", "-bso0", "-bsp0", str(OUT / "x_7zip.zip")], OUT / "x_7zip.zip"))
    if RAR.exists():
        rr = str(RAR)
        rows.append(external(files, "WinRAR 7.23: RAR5 -m5 solid -md256m", [rr, "a", "-ma5", "-m5", "-s", "-md256m", "-idq", "-ep", str(OUT / "x_rar5.rar")], OUT / "x_rar5.rar"))
        rows.append(external(files, "WinRAR 7.23: RAR5 -m5 solid -md256m -mcx (alt. search)", [rr, "a", "-ma5", "-m5", "-s", "-md256m", "-mcx", "-idq", "-ep", str(OUT / "x_rar5x.rar")], OUT / "x_rar5x.rar"))
        rows.append(external(files, "WinRAR 7.23: RAR5 -m5 solid + recovery record 10%", [rr, "a", "-ma5", "-m5", "-s", "-md256m", "-rr10p", "-idq", "-ep", str(OUT / "x_rar5rr.rar")], OUT / "x_rar5rr.rar"))
        rows.append(external(files, "WinRAR 7.23: RAR5 -m5 NON-solid", [rr, "a", "-ma5", "-m5", "-s-", "-md256m", "-idq", "-ep", str(OUT / "x_rar5ns.rar")], OUT / "x_rar5ns.rar"))
    return rows


def classic(files: list[Path]) -> list[dict]:
    rows = []
    total = sum(f.stat().st_size for f in files)

    def zip_all(method, **kw):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w", compression=method, **kw) as z:
            for f in sorted(files):
                z.write(f, f.name)
        return buf.getvalue()

    for name, fn in [
        ("zip (deflate -9, per file)", lambda: zip_all(zipfile.ZIP_DEFLATED, compresslevel=9)),
        ("zip (bzip2 -9, per file)", lambda: zip_all(zipfile.ZIP_BZIP2, compresslevel=9)),
        ("zip (lzma, per file)", lambda: zip_all(zipfile.ZIP_LZMA)),
    ]:
        out, dt = timed(fn)
        rows.append({"method": name, "bytes": len(out), "seconds": dt, "solid": False})

    tb = tar_bytes(files)
    params22 = zstd.ZstdCompressionParameters.from_level(22, window_log=27, enable_ldm=True)
    for name, fn in [
        ("tar + gzip -9", lambda: gzip.compress(tb, 9)),
        ("tar + bzip2 -9", lambda: bz2.compress(tb, 9)),
        ("tar + xz -9e (LZMA2, ~7z ultra)", lambda: lzma.compress(tb, format=lzma.FORMAT_XZ, preset=9 | lzma.PRESET_EXTREME)),
        ("tar + brotli -q 11 -w 24", lambda: brotli_cli(tb)),
        ("tar + zstd -19", lambda: zstd.ZstdCompressor(level=19).compress(tb)),
        ("tar + zstd --ultra -22 --long=27", lambda: zstd.ZstdCompressor(compression_params=params22).compress(tb)),
    ]:
        out, dt = timed(fn)
        if out is None:
            rows.append({"method": name, "bytes": None, "seconds": dt, "solid": True})
        else:
            rows.append({"method": name, "bytes": len(out), "seconds": dt, "solid": True})
    rows.extend(external_rows(files))
    for r in rows:
        r["ratio"] = (total / r["bytes"]) if r["bytes"] else None
        r["pct"] = (100.0 * r["bytes"] / total) if r["bytes"] else None
    return rows


def tsaur_variant(files: list[Path], label: str, opts: ax.PackOptions, out_name: str, verify: bool = True) -> dict:
    OUT.mkdir(exist_ok=True)
    out = OUT / out_name
    stats, dt = timed(lambda: ax.pack(files, out, opts))
    total = sum(f.stat().st_size for f in files)
    row = {"method": label, "bytes": stats["archive_bytes"], "seconds": dt, "ratio": total / stats["archive_bytes"],
           "pct": 100.0 * stats["archive_bytes"] / total, "stats": stats}
    if verify:
        t0 = time.perf_counter()
        r = ax.Reader(out, password=opts.password)
        ok = True
        for e in r.manifest["entries"]:
            data = r.extract_entry(e)
            if e["mode"] != "canonical-md":
                src = next(f for f in files if f.name == e["path"])
                ok &= (data == src.read_bytes())
        row["roundtrip_ok"] = ok
        row["unpack_seconds"] = time.perf_counter() - t0
    return row


def fmt_rows(rows: list[dict], total: int) -> str:
    lines = ["| Method | Size | % of original | Ratio | Comp. time (s) | Solid/dedup |", "|---|---:|---:|---:|---:|---|"]
    for r in rows:
        if r["bytes"] is None:
            lines.append(f"| {r['method']} | n/a | n/a | n/a | - | - |")
            continue
        extra = "solid" if r.get("solid") else ("per file" if "solid" in r else "chunk+dedup")
        lines.append(f"| {r['method']} | {r['bytes']:,} | {r['pct']:.1f}% | {r['ratio']:.2f}x | {r['seconds']:.2f} | {extra} |")
    return "\n".join(lines)


def per_file_table(files: list[Path]) -> str:
    lines = ["| File | Original | zstd -19 | xz -9e | % (xz) | Note |", "|---|---:|---:|---:|---:|---|"]
    for f in sorted(files):
        d = f.read_bytes()
        z = len(zstd.ZstdCompressor(level=19).compress(d))
        x = len(lzma.compress(d, format=lzma.FORMAT_XZ, preset=9 | lzma.PRESET_EXTREME))
        note = ""
        if f.suffix == ".docx":
            note = "ZIP container (already Deflate)"
        elif f.suffix == ".pdf":
            note = "Flate streams" + (" + images" if "brochure" in f.name else "")
        lines.append(f"| {f.name} | {len(d):,} | {z:,} | {x:,} | {100.0*x/len(d):.1f}% | {note} |")
    return "\n".join(lines)


def container_experiment(files: list[Path]) -> str:
    """How much container expansion gains (docx exploded / pdf with expanded streams) + canonical."""
    from tsaurproto import containers
    lines = ["| File | Original | Expanded container | xz(expanded) | Canonical (md) | xz(canonical) | Bit-exact? |",
             "|---|---:|---:|---:|---:|---:|---|"]
    for f in sorted(files):
        if f.suffix not in (".docx", ".pdf"):
            continue
        d = f.read_bytes()
        if f.suffix == ".docx":
            res = containers.try_bitexact_zip(d)
            if res:
                _, parts, lvl = res
                expanded = sum(len(v) for v in parts.values())
                exp_blob = b"".join(parts[k] for k in sorted(parts))
                bit = f"yes (deflate level {lvl})"
            else:
                _, parts = containers.explode_zip(d)
                expanded = sum(len(v) for v in parts.values())
                exp_blob = b"".join(parts[k] for k in sorted(parts))
                bit = "no (fallback raw)"
            md = containers.docx_to_markdown(d).encode()
        else:
            exp_blob = containers.pdf_expand_streams(d)
            expanded = len(exp_blob)
            bit = "no (requires preflate)"
            md = containers.pdf_to_markdown(d).encode()
        xz_exp = len(lzma.compress(exp_blob, format=lzma.FORMAT_XZ, preset=9 | lzma.PRESET_EXTREME))
        xz_md = len(lzma.compress(md, format=lzma.FORMAT_XZ, preset=9 | lzma.PRESET_EXTREME))
        lines.append(f"| {f.name} | {len(d):,} | {expanded:,} | {xz_exp:,} | {len(md):,} | {xz_md:,} | {bit} |")
    return "\n".join(lines)


def recovery_experiment(files: list[Path]) -> str:
    OUT.mkdir(exist_ok=True)
    out = OUT / "recovery.tsrp"
    ax.pack(files, out, ax.PackOptions(solid_block=1 << 20, password="demo-password"))
    meta = ax.write_sidecars(out, piece_size=64 * 1024, parity_ratio=0.10)
    orig = out.read_bytes()
    k, m, ps = meta["k"], meta["m"], meta["piece_size"]
    # corrupt m pieces (the maximum repairable)
    data = bytearray(orig)
    victims = list(range(0, k, max(1, k // m)))[:m]
    for i in victims:
        data[i * ps:(i + 1) * ps] = os.urandom(min(ps, len(data) - i * ps))
    out.write_bytes(bytes(data))
    bad, _ = ax.verify_pieces(out)
    fixed, ok = ax.recover(out)
    same = out.read_bytes() == orig
    par_size = out.with_suffix(".tsrp.par").stat().st_size
    meta_size = out.with_suffix(".tsrp.meta").stat().st_size
    return (f"- Encrypted archive (Argon2id + XChaCha20-Poly1305), {len(orig):,} bytes, {ps//1024} KiB pieces: k={k} data pieces, m={m} RS parity pieces"
            f" ({par_size:,} bytes of parity = {100.0*par_size/len(orig):.1f}%), .meta = {meta_size:,} bytes, Merkle root = `{meta['root'].hex()[:16]}...`\n"
            f"- Pieces deliberately corrupted: {len(bad)} ({bad}); repaired: {fixed}; verification after repair: {'OK' if ok else 'FAILED'}; bit-identical to the original: {'YES' if same else 'NO'}")


def main():
    OUT.mkdir(exist_ok=True)
    corpus = sorted(p for p in (BENCH / "corpus").iterdir() if p.is_file())
    versions = sorted(p for p in (BENCH / "corpus_versions").iterdir() if p.is_file())
    total_c = sum(f.stat().st_size for f in corpus)
    total_v = sum(f.stat().st_size for f in versions)

    md = [f"# Benchmark results (generated automatically by `prototype/bench.py`)\n",
          f"Machine: Windows 11, Python {sys.version.split()[0]}, zstandard {zstd.__version__}, zstd lib {zstd.ZSTD_VERSION}; "
          f"7-Zip 26.03 ({'found' if SEVENZIP.exists() else 'absent'}) and WinRAR 7.23 ({'found' if RAR.exists() else 'absent'}) run through their CLI. "
          f"`xz -9e` = LZMA2 from Python; all measurements are single-run, the times include the start-up of the external processes.\n",
          f"## 1. Corpus A: 10 random files (.md/.txt/.docx/.pdf), {total_c:,} bytes\n",
          "### 1.1 Per file (what compresses and what does not)\n", per_file_table(corpus), "",
          "### 1.2 Classic formats (measured locally)\n"]
    rows = classic(corpus)
    md.append(fmt_rows(rows, total_c))

    md.append("\n### 1.3 T-saur prototype (lossless bit-exact) - variants\n")
    tsaur_rows = [
        tsaur_variant(corpus, "T-saur v0 (py): chunk 8K + zstd dict, per-chunk", ax.PackOptions(), "a_chunk_dict.tsrp"),
        tsaur_variant(corpus, "T-saur v0 (py): chunk 8K, NO dict, per-chunk", ax.PackOptions(train_dict=False, codecs=(ax.CODEC_ZSTD, ax.CODEC_XZ)), "a_chunk_nodict.tsrp"),
        tsaur_variant(corpus, "T-saur v0 (py): chunk 64K avg + dict, per-chunk", ax.PackOptions(chunk_min=16384, chunk_avg=65536, chunk_max=262144), "a_chunk64_dict.tsrp"),
        tsaur_variant(corpus, "T-saur v0 (py): solid blocks 1 MiB (best-of zstd/xz), container-aware", ax.PackOptions(solid_block=1 << 20), "a_solid.tsrp"),
        tsaur_variant(corpus, "T-saur v0 (py): solid 1 MiB, container-aware OFF", ax.PackOptions(solid_block=1 << 20, container_aware=False), "a_solid_noca.tsrp"),
        tsaur_variant(corpus, "T-saur v0 (py): solid 1 MiB + XChaCha20-Poly1305/Argon2id encryption", ax.PackOptions(solid_block=1 << 20, password="demo-password"), "a_solid_enc.tsrp"),
    ]
    md.append(fmt_rows(tsaur_rows, total_c))
    md.append("\nDedup/chunking details (per-chunk 8K + dict variant):")
    s = tsaur_rows[0]["stats"]
    md.append(f"- input: {s['input_bytes']:,} bytes of original files -> {s['logical_bytes']:,} logical bytes after container expansion (DOCX -> XML); "
              f"chunks: {s['chunks_total']} total, {s['chunks_unique']} unique; internal dedup (repetitive XML, repeated images in PDF): {s['dedup_saved_bytes']:,} bytes; "
              f"dictionary: {s['dict_bytes']:,} bytes; manifest (CBOR+zstd): {s['manifest_bytes']:,} bytes; codecs chosen: {s['codec_hist']}; "
              f"bit-exact roundtrip: {tsaur_rows[0]['roundtrip_ok']}; unpack {tsaur_rows[0]['unpack_seconds']:.2f}s")
    s = tsaur_rows[3]["stats"]
    md.append(f"- solid: {s['blobs']} blocks; codecs: {s['codec_hist']}; dictionary: {s['dict_bytes']:,} bytes (disabled in solid mode); manifest {s['manifest_bytes']:,} bytes; roundtrip: {tsaur_rows[3]['roundtrip_ok']}")

    md.append("\n### 1.4 Already-compressed containers: how much expansion gains + the canonical representation for agents\n")
    md.append(container_experiment(corpus))
    md.append("\n### 1.5 'agent/canonical' mode (DOCX/PDF -> Markdown; semantic-lossless, NOT bit-exact)\n")
    canon = tsaur_variant(corpus, "T-saur v0 (py) canonical: docx/pdf -> md, solid 1 MiB", ax.PackOptions(solid_block=1 << 20, canonical=True), "a_canonical.tsrp", verify=False)
    md.append(fmt_rows([canon], total_c))
    r = ax.Reader(OUT / "a_canonical.tsrp")
    md.append("\nCanonical entries: " + ", ".join(f"{e['path']} ({e['size']:,} -> {e['canon_size']:,} bytes text)" for e in r.manifest["entries"] if e["mode"] == "canonical-md"))

    md.append(f"\n## 2. Corpus B: the same 10 files + 5 edited versions ({len(versions)} files, {total_v:,} bytes) - dedup scenario\n")
    md.append("### 2.1 Classic formats\n")
    md.append(fmt_rows(classic(versions), total_v))
    md.append("\n### 2.2 T-saur prototype\n")
    v_rows = [
        tsaur_variant(versions, "T-saur v0 (py): chunk 8K + dict, per-chunk (CDC dedup)", ax.PackOptions(), "b_chunk_dict.tsrp"),
        tsaur_variant(versions, "T-saur v0 (py): solid 1 MiB (CDC dedup + solid)", ax.PackOptions(solid_block=1 << 20), "b_solid.tsrp"),
    ]
    md.append(fmt_rows(v_rows, total_v))
    s = v_rows[0]["stats"]
    md.append(f"\n- CDC dedup: {s['chunks_total']} chunks, {s['chunks_unique']} unique; {s['dedup_saved_bytes']:,} bytes removed before compression "
              f"out of {s['logical_bytes']:,} logical ({100.0*s['dedup_saved_bytes']/s['logical_bytes']:.1f}%; original input {s['input_bytes']:,}); "
              f"manifest {s['manifest_bytes']:,} bytes; roundtrip: {v_rows[0]['roundtrip_ok']}")

    md.append("\n## 3. P2P pieces + Reed-Solomon recovery (sidecar .meta / .par)\n")
    md.append(recovery_experiment(corpus))

    md.append("\n## 4. How to read the numbers\n")
    md.append("- The corpus is ~57% PDF with images (brochure-B.pdf, 877 KB) - practically incompressible for any classic archiver; the texts (.md/.txt) compress 4-5x.\n"
              "- 'Solid' (tar+xz/zstd) beats 'per file' (zip) because it uses the context across files; T-saur with solid blocks recovers this advantage and keeps dedup + block-level access.\n"
              "- Per-chunk compression (8 KB) loses context; the trained dictionary recovers part of it (see the difference dict vs. no dict) - exactly the 'shared references' mechanism.\n"
              "- CDC dedup matters only when there is real redundancy (versions, boilerplate); on 10 unrelated files the gain is ~0. zstd --long / RAR with a large dictionary also catch redundancy in solid mode, but without granular/P2P access.\n"
              "- The canonical (agent) mode is the only one that 'breaks' the PDF/DOCX barrier: it stores the information (text+structure), not the bytes; it is explicitly declared non-bit-exact.\n")
    (BENCH / "RESULTS.md").write_text("\n".join(md), encoding="utf-8")
    print("\n".join(md))


if __name__ == "__main__":
    main()
