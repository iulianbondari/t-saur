#!/usr/bin/env python3
"""Cross-build compatibility check: every golden archive is read by BOTH release binaries
(full and lite), the outcome of list / info / verify / unpack is recorded with exit codes, and
every successful extraction is compared byte for byte with the committed inputs.

Prints a Markdown table (the one in docs/DISTRIBUTION-POLICY.md) and exits non-zero when any
outcome differs from the expectation encoded below.
"""

import filecmp
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WS = ROOT / "tsaur"
EXE = "tsaur.exe" if os.name == "nt" else "tsaur"
BUILDS = {"full": WS / "target" / "release" / EXE, "lite": WS / "target-nolepton" / "release" / EXE}
GOLDEN = WS / "crates" / "tsaur-core" / "tests" / "golden"
INPUTS = GOLDEN / "inputs"

# archive -> (extra args, expected exit codes per build for verify and unpack, restored files to compare)
CASES = {
    "full-default.tsr": ([], {"full": (0, 0), "lite": (2, 7)}, "all"),
    "lite-default.tsr": ([], {"full": (0, 0), "lite": (0, 0)}, "all"),
    "canonical.tsr": ([], {"full": (0, 0), "lite": (0, 0)}, "all"),
    "incremental.tsr": (["--ref", str(GOLDEN / "base.tsr")], {"full": (0, 0), "lite": (0, 0)}, "all"),
    "encrypted.tsr": (["--password", "golden"], {"full": (0, 0), "lite": (0, 0)}, "all"),
}


def run(exe: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(exe), *args], capture_output=True, text=True)


def same_tree(a: Path, b: Path) -> bool:
    cmp = filecmp.dircmp(a, b)
    if cmp.left_only or cmp.right_only or cmp.diff_files or cmp.funny_files:
        return False
    return all(same_tree(a / d, b / d) for d in cmp.common_dirs)


def main() -> int:
    for name, exe in BUILDS.items():
        if not exe.exists():
            print(f"missing {name} binary {exe}; build both release configurations first", file=sys.stderr)
            return 2
    failures = 0
    rows = ["| Archive | Reader | `list` | `info` (`requires`) | `verify` | `unpack` | Restored bit-exact |", "|---|---|:---:|:---:|:---:|:---:|:---:|"]
    work = Path(tempfile.mkdtemp(prefix="tsaur-compat-"))
    for archive, (extra, expect, _) in CASES.items():
        path = GOLDEN / archive
        for build, exe in BUILDS.items():
            lst = run(exe, "list", str(path), "--json", *extra)
            info = run(exe, "info", str(path), "--json", *extra)
            requires = ""
            try:
                requires = ",".join(json.loads(info.stdout).get("requires", [])) or "–"
            except json.JSONDecodeError:
                requires = "?"
            ver = run(exe, "verify", str(path), *extra)
            out = work / f"{archive}-{build}"
            shutil.rmtree(out, ignore_errors=True)
            unp = run(exe, "unpack", str(path), str(out), *extra)
            restored = same_tree(INPUTS, out) if unp.returncode == 0 else False
            exp_ver, exp_unp = expect[build]
            ok = lst.returncode == 0 and info.returncode == 0 and ver.returncode == exp_ver and unp.returncode == exp_unp and (restored or exp_unp != 0)
            if not ok:
                failures += 1
            rows.append(
                f"| `{archive}` | {build} | {'ok' if lst.returncode == 0 else 'exit ' + str(lst.returncode)} | {'ok' if info.returncode == 0 else 'exit ' + str(info.returncode)} ({requires}) | "
                f"{'ok' if ver.returncode == 0 else 'exit ' + str(ver.returncode)} | {'ok' if unp.returncode == 0 else 'exit ' + str(unp.returncode)} | {'yes' if restored else ('n/a' if exp_unp != 0 else 'NO')} |"
                + ("" if ok else "  **UNEXPECTED**")
            )
    # volumes made by one build are joined by the other, bit-exact
    for build, exe in BUILDS.items():
        joined = work / f"joined-{build}.tsr"
        j = run(exe, "volumes", "join", str(joined), str(GOLDEN / "volumes-2p1"))
        same = j.returncode == 0 and joined.read_bytes() == (GOLDEN / "full-default.tsr").read_bytes()
        if not same:
            failures += 1
        rows.append(f"| `volumes-2p1/` (split by the full build) | {build} `volumes join` | – | – | – | {'ok' if j.returncode == 0 else 'exit ' + str(j.returncode)} | {'yes' if same else 'NO'} |")
    versions = {b: run(e, "--version").stdout.strip() for b, e in BUILDS.items()}
    print(f"Readers: full = `{versions['full']}` ({BUILDS['full'].relative_to(ROOT)}), lite = `{versions['lite']}` ({BUILDS['lite'].relative_to(ROOT)})\n")
    print("\n".join(rows))
    shutil.rmtree(work, ignore_errors=True)
    print(f"\n{'all outcomes as expected' if failures == 0 else str(failures) + ' unexpected outcome(s)'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
