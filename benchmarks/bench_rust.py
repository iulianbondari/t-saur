"""Benchmark the Rust reference implementation (`tsaur` CLI) on the benchmark corpora.

Run from the repository root after `cargo build --release` in `tsaur/`:

    python benchmarks/bench_rust.py

Writes benchmarks/RESULTS-rust.md. Independent from prototype/bench.py (which covers the classic
formats and the Python prototype), so the two can be compared side by side. The optional corpora
produced by build_extra_corpora.py (corpus_real, corpus_binary, corpus_arm64) are benchmarked with
a shorter variant list when they exist. Every corpus also gets the `--effort` ladder (1..4 next to
the default's 5) with `--codec zstd` at level 19 as the explicit baseline. `RUNS=n` (default 3)
repeats every `pack` and reports the median wall and CPU time (CPU = user + system time of the
`tsaur` process on POSIX; wall only on Windows).
"""
from __future__ import annotations

import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import resource  # POSIX only
except ImportError:  # pragma: no cover - Windows
    resource = None

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "benchmarks"
TSAUR = ROOT / "tsaur" / "target" / "release" / ("tsaur.exe" if sys.platform == "win32" else "tsaur")

VARIANTS = [
    ("DEFAULT: 64 KiB chunks, 1 MiB blocks, container-aware", []),
    ("block 256 KiB (finer random access)", ["--block", "0"]),  # 0 MiB is rejected below; replaced at runtime by --block-kib
    ("solid 4 MiB, 64 KiB chunks, container-aware", ["--solid", "4"]),
    ("solid 4 MiB, fine chunks (8 KiB), container-aware", ["--solid", "4", "--chunk", "fine"]),
    ("solid 4 MiB, --no-container (plain zstd on the bytes)", ["--solid", "4", "--no-container"]),
    ("granular: one 64 KiB chunk per blob + trained dictionary", ["--granular"]),
    ("granular fine (8 KiB) + trained dictionary", ["--granular", "--chunk", "fine"]),
    ("DEFAULT + encrypted (Argon2id 64 MiB) + signed", ["--password", "bench-pw", "--kdf-memory-mib", "64", "--sign-key", "KEYFILE"]),
]

# shorter list for the optional real-world / machine-code corpora
VARIANTS_EXTRA = [
    ("DEFAULT: 64 KiB chunks, 1 MiB blocks, container-aware", []),
    ("solid 8 MiB", ["--solid", "8"]),
    ("solid 32 MiB", ["--solid", "32"]),
    ("fast: --codec zstd --level 9", ["--codec", "zstd", "--level", "9"]),
]

# the codec-choice effort ladder, run on every corpus after its variants; effort 5 is the DEFAULT row
VARIANTS_EFFORT = [
    ("baseline: --codec zstd (level 19)", ["--codec", "zstd"]),
    ("--effort 1 (zstd only)", ["--effort", "1"]),
    ("--effort 2 (zstd/xz on a sample)", ["--effort", "2"]),
    ("--effort 3 (zstd/xz/PPMd on a sample)", ["--effort", "3"]),
    ("--effort 4 (3 + xz check when PPMd wins)", ["--effort", "4"]),
]

RUNS = max(1, int(os.environ.get("RUNS", "3")))

SEVENZIP = Path(r"C:\Program Files\7-Zip\7z.exe")
RAR = Path(r"C:\Program Files\WinRAR\Rar.exe")


def external(files: list[Path], name: str, cmd: list[str], out: Path, extract_cmd: list[str] | None, work: Path) -> dict | None:
    """Run an external archiver: size, pack time, and extraction time when an extract command is given."""
    if out.exists():
        out.unlink()
    t0 = time.perf_counter()
    r = subprocess.run(cmd + [str(f) for f in sorted(files)], capture_output=True, cwd=str(work))
    t_pack = time.perf_counter() - t0
    if r.returncode != 0 or not out.exists():
        return None
    t_unpack = None
    if extract_cmd:
        ex = work / (out.stem + "_x")
        shutil.rmtree(ex, ignore_errors=True)
        ex.mkdir()
        t0 = time.perf_counter()
        subprocess.run(extract_cmd + [str(ex)], capture_output=True, cwd=str(work))
        t_unpack = time.perf_counter() - t0
    return {"name": name, "bytes": out.stat().st_size, "pack": t_pack, "unpack": t_unpack}


def external_rows(files: list[Path], work: Path) -> list[dict]:
    rows = []
    if SEVENZIP.exists():
        z = str(SEVENZIP)
        out = work / "x_lzma2.7z"
        r = external(files, "7-Zip 26.03: 7z LZMA2 -mx9 solid", [z, "a", "-t7z", "-mx9", "-m0=lzma2", "-ms=on", "-mqs=on", "-bso0", "-bsp0", str(out)], out, [z, "x", "-y", "-bso0", "-bsp0", str(out), "-o"], work)
        if r:
            rows.append(r)
        out = work / "x_auto.7z"
        r = external(files, "7-Zip 26.03: 7z -mx9 defaults (automatic BCJ/BCJ2/ARM64 filters)", [z, "a", "-t7z", "-mx9", "-bso0", "-bsp0", str(out)], out, [z, "x", "-y", "-bso0", "-bsp0", str(out), "-o"], work)
        if r:
            rows.append(r)
    if RAR.exists():
        rr = str(RAR)
        out = work / "x_rar5.rar"
        r = external(files, "WinRAR 7.23: RAR5 -m5 solid -md256m -mcx", [rr, "a", "-ma5", "-m5", "-s", "-md256m", "-mcx", "-idq", "-ep", str(out)], out, [rr, "x", "-idq", "-y", str(out)], work)
        if r:
            rows.append(r)
    return rows


def run(args: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run([str(TSAUR)] + args, capture_output=True, text=True, **kw)


def pack(out: Path, src: Path, extra: list[str], keyfile: Path) -> tuple[dict, float, float | None]:
    """Pack RUNS times; returns the report, the median wall time and the median CPU time (None on Windows)."""
    extra = [keyfile.as_posix() if a == "KEYFILE" else a for a in extra]
    if extra == ["--block", "0"]:
        extra = ["--block-kib", "256"]
    walls, cpus = [], []
    rep = None
    for _ in range(RUNS):
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN) if resource else None
        t0 = time.perf_counter()
        r = run(["pack", str(out), str(src), *extra, "--json"])
        walls.append(time.perf_counter() - t0)
        if r0 is not None:
            r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
            cpus.append((r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime))
        if r.returncode != 0:
            raise SystemExit(f"pack failed: {r.stderr}")
        rep = json.loads(r.stdout)
    return rep, statistics.median(walls), (statistics.median(cpus) if cpus else None)


def fmt_cpu(cpu: float | None) -> str:
    return "–" if cpu is None else f"{cpu:.2f}"


def roundtrip_ok(archive: Path, src: Path, password: str | None) -> tuple[bool, float, float]:
    env_args = ["--password", password] if password else []
    t0 = time.perf_counter()
    v = run(["verify", str(archive), *env_args, "--json"])
    t_verify = time.perf_counter() - t0
    out_dir = archive.parent / (archive.stem + "_out")
    shutil.rmtree(out_dir, ignore_errors=True)
    t0 = time.perf_counter()
    u = run(["unpack", str(archive), str(out_dir), *env_args])
    t_unpack = time.perf_counter() - t0
    ok = v.returncode == 0 and u.returncode == 0
    if ok:
        for f in sorted(src.iterdir()):
            g = out_dir / f.name
            if not g.exists() or g.read_bytes() != f.read_bytes():
                ok = False
                break
    return ok, t_verify, t_unpack


def main() -> None:
    if not TSAUR.exists():
        raise SystemExit(f"build the CLI first: cd tsaur && cargo build --release ({TSAUR} missing)")
    version = run(["--version"]).stdout.strip()
    tmp = Path(tempfile.mkdtemp(prefix="tsaur-bench-"))
    keyfile = tmp / "key.hex"
    run(["keygen", "--out", str(keyfile)])
    lines = [f"# Rust reference implementation — benchmark (generated by `benchmarks/bench_rust.py`)\n",
             f"`{version}`, release build, {RUNS} run(s) per variant (median wall and CPU time of the `pack` process; times include process start-up). Corpora from `build_corpus.py` (and `build_extra_corpora.py` for the optional ones). Percentages are archive size relative to the input: **lower is better**.\n"]
    corpora = ["corpus", "corpus_versions"] + [n for n in ("corpus_real", "corpus_binary", "corpus_arm64") if (BENCH / n).is_dir()]
    for corpus_name in corpora:
        src = BENCH / corpus_name
        variants = VARIANTS if corpus_name in ("corpus", "corpus_versions") else VARIANTS_EXTRA
        files = sorted(p for p in src.iterdir() if p.is_file())
        total = sum(f.stat().st_size for f in files)
        lines.append(f"\n## {corpus_name}: {len(files)} files, {total:,} bytes\n")
        lines.append("| Variant | Size | % of original | Ratio | Chunks (total/unique) | Blobs | Containers exploded/fallback | Codecs / filters | Pack (s) | Pack CPU (s) | Verify (s) | Unpack (s) | Round trip |")
        lines.append("|---|---:|---:|---:|---|---:|---|---|---:|---:|---:|---:|---|")
        work = tmp / f"ext_{corpus_name}"
        work.mkdir(exist_ok=True)
        for r in external_rows(files, work):
            lines.append(f"| {r['name']} | {r['bytes']:,} | {100.0 * r['bytes'] / total:.1f}% | {total / r['bytes']:.2f}x | – | – | – | – | {r['pack']:.2f} | – | – | {r['unpack']:.2f} | (external) |")
        ladder: dict[str, tuple[int, float, float | None]] = {}
        for i, (label, extra) in enumerate(variants + VARIANTS_EFFORT):
            out = tmp / f"{corpus_name}_{i}.tsr"
            rep, t_pack, cpu = pack(out, src, extra, keyfile)
            pw = "bench-pw" if "--password" in extra else None
            ok, t_verify, t_unpack = roundtrip_ok(out, src, pw)
            size = rep["archive_bytes"]
            codecs = " ".join(f"{k}:{v}" for k, v in sorted(rep["codec_hist"].items()))
            if rep.get("blocks_sampled"):
                codecs += f" sampled:{rep['blocks_sampled']}"
            lines.append(
                f"| {label} | {size:,} | {100.0 * size / total:.1f}% | {total / size:.2f}x | {rep['chunks_total']}/{rep['chunks_unique']} | {rep['blobs']} | "
                f"{rep['containers_exploded']}/{rep['containers_fallback']} | {codecs} | {t_pack:.2f} | {fmt_cpu(cpu)} | {t_verify:.2f} | {t_unpack:.2f} | {'bit-exact OK' if ok else 'FAILED'} |"
            )
            if i == 0 or (label, extra) in VARIANTS_EFFORT:
                ladder[label] = (size, t_pack, cpu)
        # effort 3 against the default (effort 5) and the zstd baseline: ratio loss in points of the input, time ratio
        default_label, zstd_label, e3_label = variants[0][0], VARIANTS_EFFORT[0][0], VARIANTS_EFFORT[3][0]
        (s5, _, _), (sz, wz, cz), (s3, w3, c3) = ladder[default_label], ladder[zstd_label], ladder[e3_label]
        time_ratio = f"{c3 / cz:.2f}× CPU" if c3 is not None and cz else f"{w3 / wz:.2f}× wall"
        lines.append(f"\n`--effort 3` vs the default (effort 5): {100.0 * (s3 - s5) / total:+.2f} pt of the input; {time_ratio} of `--codec zstd` (target: ≤ 0.5 pt, ≤ 2×).")
    # pieces + recovery demo on the encrypted archive of the first corpus (last variant)
    arch = tmp / f"corpus_{len(VARIANTS) - 1}.tsr"  # last VARIANTS entry of the first corpus = encrypted + signed
    p = run(["pieces", str(arch), "--parity-pct", "20", "--json"])
    meta = json.loads(p.stdout)
    data = bytearray(arch.read_bytes())
    import os
    for off in (70_000, 200_000, 330_000):
        if off + 50 < len(data):
            data[off:off + 50] = os.urandom(50)
    arch.write_bytes(bytes(data))
    v1 = json.loads(run(["verify", str(arch), "--password", "bench-pw", "--pieces", "--json"]).stdout)
    rec = json.loads(run(["recover", str(arch), "--json"]).stdout)
    v2 = json.loads(run(["verify", str(arch), "--password", "bench-pw", "--pieces", "--json"]).stdout)
    lines.append("\n## Pieces + Reed-Solomon recovery (encrypted, signed solid archive of `corpus`)\n")
    lines.append(f"- {meta['pieces']} pieces x {meta['piece_size'] // 1024} KiB, {meta['parity']} parity pieces (20 %), Merkle root `{meta['root'][:16]}…`")
    lines.append(f"- after corrupting 3 pieces: verify reports bad pieces {v1['pieces']['bad']} (recoverable: {v1['pieces']['recoverable']}); recover repaired {rec['repaired']}; verify afterwards ok = {v2['ok']}, entries ok = {v2.get('entries_ok')}/{v2.get('entries_total')}")
    # offline volume set demo: 4 data + 2 parity volumes of the default archive of `corpus`, lose two drives, rebuild
    arch = tmp / "corpus_0.tsr"
    drives = [tmp / f"drive{i}" for i in range(6)]
    for d in drives:
        d.mkdir(exist_ok=True)
    t0 = time.perf_counter()
    sp = json.loads(run(["volumes", "split", str(arch), "--data", "4", "--parity", "2", "--json", *sum((["--out", str(d)] for d in drives), [])]).stdout)
    t_split = time.perf_counter() - t0
    vol_bytes = sum(v["bytes"] for v in sp["volumes"])
    shutil.rmtree(drives[1])
    shutil.rmtree(drives[4])
    insp = json.loads(run(["volumes", "inspect", *[str(d) for d in drives if d.exists()], "--verify", "--json"]).stdout)[0]
    joined = tmp / "joined.tsr"
    t0 = time.perf_counter()
    jr = json.loads(run(["volumes", "join", str(joined), *[str(d) for d in drives if d.exists()], "--json"]).stdout)
    t_join = time.perf_counter() - t0
    identical = joined.read_bytes() == arch.read_bytes()
    lines.append("\n## Offline volume set (default archive of `corpus`, 4 data + 2 parity volumes, adaptive piece size)\n")
    lines.append(f"- archive {sp['archive_size']:,} bytes -> 6 volumes totalling {vol_bytes:,} bytes ({100.0 * vol_bytes / sp['archive_size']:.0f} % of the archive: parity 2/4 = 50 % plus piece padding and descriptors) with the adaptive piece size of {sp['piece_size'] // 1024} KiB, split in {t_split:.2f} s")
    lines.append(f"- two of six drives removed: inspect --verify reports missing volumes {[i + 1 for i in insp['missing']]}, reconstructible = {insp['reconstructible']}")
    lines.append(f"- join from the remaining four in {t_join:.2f} s, {jr['pieces_rebuilt']} pieces rebuilt from parity, archive hash verified = {jr['archive_hash_ok']}, bit-identical to the original = {identical}")
    (BENCH / "RESULTS-rust.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
