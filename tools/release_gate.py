#!/usr/bin/env python3
"""The release gate: runs every producer-side check of the release candidate and writes a report
with the exact commands, exit codes, durations and key results
(docs/review/RC<n>-VERIFICATION-REPORT.md). It publishes nothing and touches no remote.

Steps: environment, rustfmt, clippy (both builds), release builds (both), test suites (both,
with a longer robustness campaign in the default build), golden-archive manifest check,
cross-build compatibility (tools/compat_check.py), the guide executed with both packaged
binaries (tools/check_guide.py), cargo package / publish --dry-run (recorded, never published),
release packages (tools/package.py).

Usage: python tools/release_gate.py [--robustness-seconds N] [--report docs/review/RC1-VERIFICATION-REPORT.md]
"""

import argparse
import hashlib
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WS = ROOT / "tsaur"
EXE = "tsaur.exe" if os.name == "nt" else "tsaur"
FULL = WS / "target" / "release" / EXE
LITE = WS / "target-nolepton" / "release" / EXE


class Step:
    def __init__(self, title: str, cmd: list[str], cwd: Path, env: dict | None = None, must_pass: bool = True, keep: int = 12, grep: str | None = None):
        self.title, self.cmd, self.cwd, self.env, self.must_pass, self.keep, self.grep = title, cmd, cwd, env, must_pass, keep, grep
        self.code = None
        self.seconds = 0.0
        self.lines: list[str] = []

    def run(self) -> None:
        env = dict(os.environ)
        if self.env:
            env.update(self.env)
        t0 = time.perf_counter()
        r = subprocess.run(self.cmd, cwd=self.cwd, env=env, capture_output=True, text=True, errors="replace")
        self.seconds = time.perf_counter() - t0
        self.code = r.returncode
        out = (r.stdout + "\n" + r.stderr).splitlines()
        if self.grep:
            out = [l for l in out if any(g in l for g in self.grep.split("|"))]
        self.lines = [l.rstrip() for l in out if l.strip()][-self.keep :]

    def ok(self) -> bool:
        return self.code == 0 or not self.must_pass


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def golden_manifest_ok() -> tuple[bool, str]:
    golden = WS / "crates" / "tsaur-core" / "tests" / "golden"
    listed = {}
    for line in (golden / "MANIFEST.sha256").read_text(encoding="utf-8").splitlines():
        digest, rel = line.split("  ", 1)
        listed[rel] = digest
    bad = [rel for rel, digest in listed.items() if not (golden / rel).exists() or sha256(golden / rel) != digest]
    return (not bad, f"{len(listed)} golden files listed, {len(bad)} mismatching{': ' + ', '.join(bad) if bad else ''}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--robustness-seconds", type=int, default=60)
    ap.add_argument("--report", default=str(ROOT / "docs" / "review" / "RC1-VERIFICATION-REPORT.md"))
    a = ap.parse_args()
    py = sys.executable
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()
    cargo = subprocess.run(["cargo", "--version"], capture_output=True, text=True).stdout.strip()
    steps = [
        Step("rustfmt", ["cargo", "fmt", "--all", "--", "--check"], WS),
        Step("clippy, default build", ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"], WS, grep="warning|error|Finished"),
        Step("clippy, lite build", ["cargo", "clippy", "--all-targets", "--no-default-features", "--target-dir", "target-nolepton", "--", "-D", "warnings"], WS, grep="warning|error|Finished"),
        Step("release build, full", ["cargo", "build", "--release"], WS, grep="Finished|error"),
        Step("release build, lite", ["cargo", "build", "--release", "--no-default-features", "--target-dir", "target-nolepton"], WS, grep="Finished|error"),
        Step("tests, default build", ["cargo", "test", "--no-fail-fast"], WS, grep="test result|Running|FAILED|panicked", keep=40),
        Step("tests, lite build", ["cargo", "test", "--no-fail-fast", "--no-default-features", "--target-dir", "target-nolepton"], WS, grep="test result|Running|FAILED|panicked", keep=40),
        Step(f"robustness campaign, {a.robustness_seconds} s per target, release profile", ["cargo", "test", "--release", "--test", "robustness", "--", "--nocapture", "--test-threads=1"], WS, env={"TSAUR_ROBUSTNESS_SECONDS": str(a.robustness_seconds)}, grep="robustness |test result|FAILED|panicked", keep=20),
        Step("cross-build compatibility of the golden archives", [py, str(ROOT / "tools" / "compat_check.py")], ROOT, keep=20),
        Step("guide executed with the full binary", [py, str(ROOT / "tools" / "check_guide.py"), "--tsaur", str(FULL)], ROOT, keep=6),
        Step("guide executed with the lite binary", [py, str(ROOT / "tools" / "check_guide.py"), "--tsaur", str(LITE)], ROOT, keep=6),
        Step("cargo package --list (core)", ["cargo", "package", "--list", "-p", "tsaur-core"], WS, keep=40),
        Step("cargo package --list (cli)", ["cargo", "package", "--list", "-p", "tsaur"], WS, keep=20),
        Step("cargo package (core, builds the packaged crate)", ["cargo", "package", "-p", "tsaur-core", "--allow-dirty"], WS, grep="Packag|Verif|Compil|Finished|error|warning", keep=12),
        Step("cargo publish --dry-run (core; recorded, nothing is published)", ["cargo", "publish", "--dry-run", "-p", "tsaur-core", "--allow-dirty"], WS, must_pass=False, grep="Packag|Verif|Upload|error|warning|Finished", keep=12),
        Step("cargo publish --dry-run (cli; expected to fail until tsaur-core is published)", ["cargo", "publish", "--dry-run", "-p", "tsaur", "--allow-dirty"], WS, must_pass=False, grep="Packag|Verif|Upload|error|warning|Finished", keep=12),
        Step("release packages built and checked from a clean directory", [py, str(ROOT / "tools" / "package.py")], ROOT, keep=8),
    ]
    results = []
    for s in steps:
        print(f"== {s.title} ...", flush=True)
        s.run()
        print(f"   exit {s.code} in {s.seconds:.1f} s" + ("" if s.ok() else "  <-- FAILED"), flush=True)
        results.append(s)
        if not s.ok():
            break
    golden_ok, golden_note = golden_manifest_ok()
    all_ok = all(s.ok() for s in results) and len(results) == len(steps) and golden_ok

    lines = [
        "# Release candidate verification report (RC1)",
        "",
        "These are the **producer's own checks** of the release candidate, run by `tools/release_gate.py`;",
        "they are not an independent review (`docs/review/REVIEW-PACKAGE.md` describes that separate step).",
        "",
        "| Field | Value |",
        "|---|---|",
        f"| date | {time.strftime('%Y-%m-%d %H:%M')} |",
        f"| commit under test | `{head}` {'(working tree clean)' if not dirty else '(working tree had uncommitted changes: ' + str(len(dirty.splitlines())) + ' paths; the report itself and generated outputs are expected among them)'} |",
        f"| platform | {platform.platform()}, {platform.machine()}, {os.cpu_count()} logical CPUs |",
        f"| toolchain | {rustc}; {cargo} |",
        f"| binaries | full `{FULL.relative_to(ROOT)}`, lite `{LITE.relative_to(ROOT)}` |",
        f"| golden archives | {golden_note} |",
        f"| overall | **{'all steps passed' if all_ok else 'FAILED'}** |",
        "",
        "## Steps",
        "",
    ]
    def short(text: str) -> str:
        """Paths of this machine are not part of the record: show them relative to the repository."""
        return text.replace(str(ROOT) + os.sep, "").replace(str(ROOT), ".").replace(sys.executable, "python")

    for s in results:
        lines += [f"### {s.title}", "", f"`{short(' '.join(str(c) for c in s.cmd))}` (in `{s.cwd.relative_to(ROOT) if s.cwd != ROOT else '.'}`)" + (f", env `{s.env}`" if s.env else ""), "",
                  f"Exit code {s.code}, {s.seconds:.1f} s" + ("" if s.ok() else " — **FAILED**") + ("" if s.must_pass else " (informational step)"), ""]
        if s.lines:
            lines += ["```", *[short(l) for l in s.lines], "```", ""]
    if len(results) < len(steps):
        lines += ["Steps not run because an earlier step failed: " + ", ".join(s.title for s in steps[len(results):]), ""]
    Path(a.report).parent.mkdir(parents=True, exist_ok=True)
    Path(a.report).write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"\nreport: {a.report}\noverall: {'all steps passed' if all_ok else 'FAILED'}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
