#!/usr/bin/env python3
"""Create the golden archives from the committed inputs with the release binaries of both builds.

Run once when the format is frozen (and never again unless the format changes, which v1.x may
not do): `cargo build --release` and `cargo build --release --no-default-features --target-dir
target-nolepton` first, then `python tests/golden/make_golden.py` from `tsaur/crates/tsaur-core`.
Writes `MANIFEST.sha256` with the SHA-256 of every golden file.
"""

import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
INPUTS = HERE / "inputs"
WS = HERE.parents[3]  # tsaur/
EXE = "tsaur.exe" if os.name == "nt" else "tsaur"
FULL = WS / "target" / "release" / EXE
LITE = WS / "target-nolepton" / "release" / EXE


def run(exe: Path, *args: str) -> None:
    r = subprocess.run([str(exe), *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"{exe.name} {' '.join(args)} failed ({r.returncode}): {r.stderr.strip()}")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    for exe in (FULL, LITE):
        if not exe.exists():
            raise SystemExit(f"missing {exe}: build both release configurations first")
    os.chdir(HERE)
    for old in ["full-default.tsr", "lite-default.tsr", "canonical.tsr", "base.tsr", "incremental.tsr", "encrypted.tsr"]:
        Path(old).unlink(missing_ok=True)
    for d in ("volumes-2p1", "volumes-2p0"):
        shutil.rmtree(d, ignore_errors=True)
    run(FULL, "pack", "full-default.tsr", "inputs")
    run(LITE, "pack", "lite-default.tsr", "inputs")
    run(FULL, "pack", "check-no-lepton.tsr", "inputs", "--no-lepton")
    if Path("check-no-lepton.tsr").read_bytes() != Path("lite-default.tsr").read_bytes():
        raise SystemExit("full build with --no-lepton and lite build produced different archives")
    Path("check-no-lepton.tsr").unlink()
    run(FULL, "pack", "canonical.tsr", "inputs", "--no-lepton", "--canonical")
    run(FULL, "pack", "base.tsr", "inputs/notes.md", "inputs/random.bin", "inputs/report.docx", "inputs/paper.pdf", "inputs/photo.jpg", "--no-lepton")
    run(FULL, "pack", "incremental.tsr", "inputs", "--no-lepton", "--ref", "base.tsr")
    if not Path("sign.key").exists():
        run(FULL, "keygen", "--out", "sign.key")
    run(FULL, "pack", "encrypted.tsr", "inputs", "--no-lepton", "--password", "golden", "--sign-key", "sign.key")
    Path("volumes-2p1").mkdir()
    run(FULL, "volumes", "split", "full-default.tsr", "--data", "2", "--parity", "1", "--piece-size-kib", "64", "--out", "volumes-2p1")
    Path("volumes-2p0").mkdir()
    run(LITE, "volumes", "split", "full-default.tsr", "--data", "2", "--parity", "0", "--piece-size-kib", "64", "--out", "volumes-2p0")
    lines = []
    for p in sorted(HERE.rglob("*")):
        if p.is_file() and p.name not in ("MANIFEST.sha256", "make_golden.py", "make_inputs.py"):
            rel = p.relative_to(HERE).as_posix()
            lines.append(f"{sha256(p)}  {rel}")
            print(f"{p.stat().st_size:8} B  {rel}")
    (HERE / "MANIFEST.sha256").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(lines)} files listed in MANIFEST.sha256")


if __name__ == "__main__":
    sys.exit(main())
