"""Build the optional benchmark corpora that are not stored in the repository.

    python benchmarks/build_extra_corpora.py [real] [binary] [arm64]

* corpus_real    real-world DOCX / PDF / JPEG files shipped with open-source packages that are
                 already installed on the machine (python-docx, matplotlib, scikit-learn) or present
                 in the Cargo registry; only files that are found are copied, the list is printed.
* corpus_binary  native x86-64 executables: a few Windows system DLLs (or Linux shared libraries)
                 plus the `tsaur` release binary itself.
* corpus_arm64   ARM64 machine code: the `.so` files of the public `zstandard` wheels for
                 macOS arm64 and Linux aarch64, downloaded with `pip download` (BSD-3 licensed).

Nothing here is redistributed; `bench_rust.py` benchmarks whichever `corpus*` directories exist.
"""
from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

BENCH = Path(__file__).resolve().parent
ROOT = BENCH.parent
TSAUR = ROOT / "tsaur" / "target" / "release" / ("tsaur.exe" if sys.platform == "win32" else "tsaur")


def package_dir(module: str) -> Path | None:
    spec = importlib.util.find_spec(module)
    return Path(spec.origin).parent if spec and spec.origin else None


def fresh(name: str) -> Path:
    d = BENCH / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir()
    return d


def build_real() -> None:
    out = fresh("corpus_real")
    candidates: list[Path] = []
    if (d := package_dir("docx")) is not None:
        candidates.append(d / "templates" / "default.docx")
    if (d := package_dir("matplotlib")) is not None:
        candidates.append(d / "mpl-data" / "sample_data" / "grace_hopper.jpg")
        candidates += sorted((d / "mpl-data" / "images").glob("*.pdf"))[:8]
    if (d := package_dir("sklearn")) is not None:
        candidates += [d / "datasets" / "images" / "china.jpg", d / "datasets" / "images" / "flower.jpg"]
    registry = Path.home() / ".cargo" / "registry" / "src"
    for pattern in ("*/zlib-rs-*/**/paper-100k.pdf", "*/lepton_jpeg-*/images/*.jpg", "*/image-*/tests/images/jpg/*.jpg"):
        candidates += sorted(registry.glob(pattern))[:4]
    found = 0
    for c in candidates:
        if c.is_file() and 1_000 < c.stat().st_size < 4_000_000:
            shutil.copy2(c, out / c.name)
            found += 1
            print(f"  {c.name:<45} {c.stat().st_size:>9,} B  <- {c}")
    print(f"corpus_real: {found} files")


def build_binary() -> None:
    out = fresh("corpus_binary")
    if sys.platform == "win32":
        sys32 = Path(r"C:\Windows\System32")
        sources = [sys32 / n for n in ("kernel32.dll", "user32.dll", "ntdll.dll", "msvcrt.dll")]
    else:
        sources = [Path(p) for p in ("/usr/lib/x86_64-linux-gnu/libc.so.6", "/usr/lib/x86_64-linux-gnu/libstdc++.so.6", "/usr/lib/aarch64-linux-gnu/libc.so.6")]
    sources.append(TSAUR)
    found = 0
    for s in sources:
        if s.is_file():
            shutil.copy2(s, out / s.name)
            found += 1
            print(f"  {s.name:<30} {s.stat().st_size:>10,} B  <- {s}")
    print(f"corpus_binary: {found} files")


def build_arm64() -> None:
    out = fresh("corpus_arm64")
    wheels = BENCH / "_wheels"
    wheels.mkdir(exist_ok=True)
    for platform in ("macosx_11_0_arm64", "manylinux2014_aarch64"):
        subprocess.run([sys.executable, "-m", "pip", "download", "zstandard", "--platform", platform, "--only-binary=:all:", "--no-deps", "-d", str(wheels), "-q"], check=False)
    found = 0
    for w in sorted(wheels.glob("zstandard-*.whl")):
        tag = "mac" if "macosx" in w.name else "linux"
        with zipfile.ZipFile(w) as z:
            for n in z.namelist():
                if n.endswith(".so"):
                    data = z.read(n)
                    (out / f"{tag}-{Path(n).name}").write_bytes(data)
                    found += 1
                    print(f"  {tag}-{Path(n).name:<45} {len(data):>10,} B  <- {w.name}")
    shutil.rmtree(wheels, ignore_errors=True)
    print(f"corpus_arm64: {found} files")


def main() -> None:
    which = set(sys.argv[1:]) or {"real", "binary", "arm64"}
    if "real" in which:
        build_real()
    if "binary" in which:
        build_binary()
    if "arm64" in which:
        build_arm64()


if __name__ == "__main__":
    main()
