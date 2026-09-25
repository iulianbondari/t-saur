#!/usr/bin/env python3
"""Assemble the release packages for this platform from the two release builds, then verify each
package from a clean directory (unpack the zip somewhere else, run the packaged binary through
the guide's commands). Nothing is uploaded anywhere.

Output (in dist/): tsaur-<version>-<platform>-lite.zip, tsaur-<version>-<platform>-full.zip,
SHA256SUMS, and a PACKAGES.md summary with sizes and hashes.

Requires both builds: `cargo build --release` and
`cargo build --release --no-default-features --target-dir target-nolepton` (in tsaur/).
"""

import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WS = ROOT / "tsaur"
EXE = "tsaur.exe" if os.name == "nt" else "tsaur"
BUILDS = {"full": WS / "target" / "release" / EXE, "lite": WS / "target-nolepton" / "release" / EXE}
DIST = ROOT / "dist"

COMMON_FILES = ["README.md", "LICENSE-APACHE", "LICENSE-MIT", "THIRD-PARTY-NOTICES.md", "CHANGELOG.md", "SECURITY.md",
                "docs/GUIDE.md", "docs/V1-CONTRACT.md", "docs/DISTRIBUTION-POLICY.md", "docs/spec/TSAUR-FORMAT-SPEC-v1.0.md",
                "docs/design/VOLUME-SETS.md", "docs/design/VOLUME-TRUST.md"]
# the LGPL texts go into every package: `cabac` (LGPL-3.0-or-later) is part of every build through `preflate-rs`
LGPL_TEXTS = ["licenses/LGPL-3.0.txt", "licenses/GPL-3.0.txt"]


def version(exe: Path) -> str:
    return subprocess.run([str(exe), "--version"], capture_output=True, text=True).stdout.split()[-1]


def platform_tag() -> str:
    system = {"Windows": "windows", "Linux": "linux", "Darwin": "macos"}.get(platform.system(), platform.system().lower())
    arch = {"AMD64": "x64", "x86_64": "x64", "arm64": "arm64", "aarch64": "arm64"}.get(platform.machine(), platform.machine().lower())
    return f"{system}-{arch}"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def package_readme(variant: str, ver: str, tag: str) -> str:
    lepton = ("This is the **full** build: it recompresses JPEG files and PDF images losslessly with Lepton. Like every "
              "build it contains `cabac` (LGPL-3.0-or-later) through `preflate-rs` (see THIRD-PARTY-NOTICES.md and licenses/). Archives that contain "
              "recompressed JPEGs need this build to open; `tsaur pack --no-lepton` writes archives every build can read."
              if variant == "full" else
              "This is the **lite** build: no Lepton, JPEGs are stored as they are, and every archive it writes can be "
              "opened by any T-saur build. It cannot open archives whose JPEG entries were recompressed by the full build "
              "(they report `requires: lepton`).")
    return f"""# T-saur {ver} — {variant} build for {tag}

{lepton}

## Install

1. Verify the download: compare the SHA-256 of the zip with the value in `SHA256SUMS`.
2. Put `{EXE}` on your PATH (or run it by path).
3. `tsaur --version`, then follow `docs/GUIDE.md` (pack, verify, unpack, volumes, transfer).

## Contents

`{EXE}`, README.md, docs/GUIDE.md, docs/V1-CONTRACT.md (what v1.0 promises), docs/DISTRIBUTION-POLICY.md,
the format specification, the volume-set and trust design notes, CHANGELOG.md, SECURITY.md, the licenses
(Apache-2.0 OR MIT for T-saur; THIRD-PARTY-NOTICES.md for the dependencies, licenses/ for the LGPL texts of `cabac`, which every build contains through `preflate-rs`).

## Quick check

```
tsaur pack demo.tsr <some directory>
tsaur verify demo.tsr
tsaur unpack demo.tsr restored/
```

Everything works offline. No account, service or network is needed for any command; `volumes serve`
listens on loopback unless told otherwise.
"""


def build_package(variant: str, exe: Path, ver: str, tag: str, stage: Path) -> Path:
    pkg = stage / f"tsaur-{ver}-{tag}-{variant}"
    shutil.rmtree(pkg, ignore_errors=True)
    pkg.mkdir(parents=True)
    shutil.copy2(exe, pkg / EXE)
    for rel in COMMON_FILES + LGPL_TEXTS:
        src = ROOT / rel
        dst = pkg / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
    (pkg / "README-PACKAGE.md").write_text(package_readme(variant, ver, tag), encoding="utf-8")
    zip_path = DIST / f"{pkg.name}.zip"
    zip_path.unlink(missing_ok=True)
    with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_DEFLATED) as z:
        for p in sorted(pkg.rglob("*")):
            if p.is_file():
                z.write(p, f"{pkg.name}/{p.relative_to(pkg).as_posix()}")
    return zip_path


def verify_package(zip_path: Path) -> tuple[bool, str]:
    """Unpack the zip in a clean temporary directory and run the guide against the packaged binary."""
    clean = Path(tempfile.mkdtemp(prefix="tsaur-pkg-check-"))
    with zipfile.ZipFile(zip_path) as z:
        z.extractall(clean)
    inner = next(clean.iterdir())
    exe = inner / EXE
    if os.name != "nt":
        exe.chmod(0o755)
    v = subprocess.run([str(exe), "--version"], capture_output=True, text=True)
    guide = subprocess.run([sys.executable, str(ROOT / "tools" / "check_guide.py"), "--tsaur", str(exe), "--guide", str(inner / "docs" / "GUIDE.md")], capture_output=True, text=True)
    ok = v.returncode == 0 and guide.returncode == 0
    summary = f"{v.stdout.strip()}; guide: {guide.stdout.strip().splitlines()[-1] if guide.stdout.strip() else guide.stderr.strip()[-300:]}"
    shutil.rmtree(clean, ignore_errors=True)
    return ok, summary


def main() -> int:
    for name, exe in BUILDS.items():
        if not exe.exists():
            print(f"missing {name} binary {exe}", file=sys.stderr)
            return 2
    DIST.mkdir(exist_ok=True)
    ver = version(BUILDS["full"])
    tag = platform_tag()
    stage = Path(tempfile.mkdtemp(prefix="tsaur-stage-"))
    rows = []
    all_ok = True
    sums = []
    for variant in ("lite", "full"):
        zip_path = build_package(variant, BUILDS[variant], ver, tag, stage)
        digest = sha256(zip_path)
        sums.append(f"{digest}  {zip_path.name}")
        ok, summary = verify_package(zip_path)
        all_ok &= ok
        rows.append(f"| `{zip_path.name}` | {zip_path.stat().st_size:,} | `{digest}` | {'ok' if ok else 'FAILED'}: {summary} |")
        print(f"{zip_path.name}: {zip_path.stat().st_size:,} bytes, sha256 {digest}, clean-directory check {'ok' if ok else 'FAILED'} ({summary})")
    (DIST / "SHA256SUMS").write_text("\n".join(sums) + "\n", encoding="utf-8")
    (DIST / "PACKAGES.md").write_text(
        f"# Packages for {tag}, version {ver}\n\nBuilt on {platform.platform()} from commit "
        f"{subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, capture_output=True, text=True).stdout.strip()}.\n\n"
        "| Package | Bytes | SHA-256 | Clean-directory check (unzip elsewhere, run the guide) |\n|---|---:|---|---|\n" + "\n".join(rows) + "\n",
        encoding="utf-8",
    )
    shutil.rmtree(stage, ignore_errors=True)
    print(f"\nwritten: {(DIST / 'SHA256SUMS').relative_to(ROOT)}, {(DIST / 'PACKAGES.md').relative_to(ROOT)}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
