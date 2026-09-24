#!/usr/bin/env python3
"""Run every runnable `bash` block of docs/GUIDE.md, in order, in a fresh directory, with a given
`tsaur` binary on PATH. A block preceded (within its paragraph) by the words "not run
automatically" is skipped. Exit codes are checked by the blocks themselves (`bash -e`: any
failing command stops the block and fails the check).

Usage: python tools/check_guide.py [--tsaur PATH] [--guide docs/GUIDE.md]
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = "tsaur.exe" if os.name == "nt" else "tsaur"
DEFAULT_TSAUR = ROOT / "tsaur" / "target" / "release" / EXE


def blocks(text: str) -> list[tuple[int, str, bool]]:
    """(line number, code, runnable) for every ```bash block."""
    out = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if lines[i].strip() == "```bash":
            start = i + 1
            j = start
            while j < len(lines) and lines[j].strip() != "```":
                j += 1
            code = "\n".join(lines[start:j])
            # the paragraph before the block (skipping blank lines) may carry the skip marker
            k = i - 1
            while k >= 0 and lines[k].strip() == "":
                k -= 1
            context = []
            while k >= 0 and lines[k].strip() != "":
                context.append(lines[k])
                k -= 1
            runnable = "not run automatically" not in " ".join(context)
            out.append((start, code, runnable))
            i = j + 1
        else:
            i += 1
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tsaur", default=str(DEFAULT_TSAUR))
    ap.add_argument("--guide", default=str(ROOT / "docs" / "GUIDE.md"))
    a = ap.parse_args()
    exe = Path(a.tsaur).resolve()
    if not exe.exists() and exe.with_name(exe.name + ".exe").exists():
        exe = exe.with_name(exe.name + ".exe")  # the same command line works on Windows
    if not exe.exists():
        print(f"binary not found: {exe}", file=sys.stderr)
        return 2
    bash = shutil.which("bash")
    if not bash:
        print("bash is required (Git Bash on Windows)", file=sys.stderr)
        return 2
    work = Path(tempfile.mkdtemp(prefix="tsaur-guide-"))
    bindir = work / "bin"
    bindir.mkdir()
    shutil.copy2(exe, bindir / EXE)
    env = dict(os.environ)
    env["PATH"] = str(bindir) + os.pathsep + env.get("PATH", "")
    text = Path(a.guide).read_text(encoding="utf-8")
    ran = skipped = 0
    for line_no, code, runnable in blocks(text):
        if not runnable:
            skipped += 1
            print(f"-- block at line {line_no}: skipped (not run automatically)")
            continue
        # placeholders like <fingerprint> only appear in blocks that are not run
        if re.search(r"<[a-z ]+>", code):
            print(f"-- block at line {line_no}: contains placeholders, marked runnable: FAIL", file=sys.stderr)
            return 1
        r = subprocess.run([bash, "-e", "-c", code], cwd=work, env=env, capture_output=True, text=True)
        ran += 1
        status = "ok" if r.returncode == 0 else f"FAILED (exit {r.returncode})"
        print(f"-- block at line {line_no}: {status}")
        for out_line in (r.stdout.strip().splitlines())[-6:]:
            print("   " + out_line)
        if r.returncode != 0:
            print(r.stderr.strip()[-2000:], file=sys.stderr)
            print(f"working directory kept for inspection: {work}", file=sys.stderr)
            return 1
    shutil.rmtree(work, ignore_errors=True)
    shown = exe.relative_to(ROOT) if exe.is_relative_to(ROOT) else exe.name
    print(f"{ran} block(s) ran, {skipped} skipped, binary {shown}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
