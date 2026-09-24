#!/usr/bin/env python3
"""Two-device transfer measurements for T-saur volume sets.

Plan: docs/design/TWO-DEVICE-BENCHMARK-PLAN.md. Form: benchmarks/TWO-DEVICE-RESULTS-TEMPLATE.md.

Roles (one machine is A, the other is B):

  prepare  (A)  build the corpora, pack them, split 4+2, create A's identity, write A/manifest.json
  serve    (A)  run `tsaur volumes serve` for the split volumes (foreground; Ctrl-C stops it)
  keygen   (B)  create B's identity and print its fingerprint (for A's --allow)
  fetch    (B)  run every measurement of the plan against A, write results JSON + markdown

Nothing here talks to any service. The manifest (set ids, descriptor hashes, archive hashes, A's
fingerprint) is copied from A to B by hand: that copy is the trusted channel of the trust contract.
A run with both roles on one machine must be started with --same-machine-check; it is then labelled
as a tooling check, because it measures nothing in the plan.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "benchmarks"
DEFAULT_TSAUR = ROOT / "tsaur" / "target" / "release" / ("tsaur.exe" if os.name == "nt" else "tsaur")

CORPORA = {
    # name: (description, input builder)
    "S": "benchmarks/corpus (the default benchmark corpus)",
    "M": "benchmarks/corpus_versions plus corpus_real when present",
    "L": "generated: 200 MB of pseudo-text plus 60 MB of noise (deterministic)",
}


# ------------------------------------------------------------------ helpers


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_commit() -> str:
    try:
        return subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    except Exception:
        return "unknown"


def tsaur_version(tsaur: Path) -> str:
    try:
        return subprocess.run([str(tsaur), "--version"], capture_output=True, text=True).stdout.strip()
    except Exception:
        return "unknown"


def is_loopback(addr: str) -> bool:
    host = addr.rsplit(":", 1)[0].strip("[]")
    return host in ("127.0.0.1", "::1", "localhost") or host.startswith("127.")


def peak_rss(pid: int) -> int | None:
    """Peak resident set of a live process, in bytes (Windows and Linux; None elsewhere)."""
    if os.name == "nt":
        import ctypes
        import ctypes.wintypes as wt

        class PMC(ctypes.Structure):
            _fields_ = [
                ("cb", wt.DWORD),
                ("PageFaultCount", wt.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t),
            ]

        handle = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return None
        try:
            pmc = PMC()
            pmc.cb = ctypes.sizeof(PMC)
            if ctypes.windll.psapi.GetProcessMemoryInfo(handle, ctypes.byref(pmc), pmc.cb):
                return int(pmc.PeakWorkingSetSize)
            return None
        finally:
            ctypes.windll.kernel32.CloseHandle(handle)
    if sys.platform.startswith("linux"):
        try:
            for line in open(f"/proc/{pid}/status", encoding="ascii", errors="replace"):
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
        except OSError:
            return None
    return None


class Sampler:
    """Polls a child's peak RSS every 50 ms until it exits."""

    def __init__(self, proc: subprocess.Popen):
        self.proc = proc
        self.peak: int | None = None
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self) -> None:
        while self.proc.poll() is None:
            v = peak_rss(self.proc.pid)
            if v is not None and (self.peak is None or v > self.peak):
                self.peak = v
            time.sleep(0.05)

    def finish(self) -> int | None:
        self.thread.join()
        return self.peak


def run_measured(cmd: list[str]) -> tuple[int, str, str, float, int | None]:
    """Run a command, returning (exit code, stdout, stderr, wall seconds, peak RSS bytes)."""
    t0 = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    sampler = Sampler(proc)
    out, err = proc.communicate()
    dt = time.perf_counter() - t0
    return proc.returncode, out, err, dt, sampler.finish()


def run_json(tsaur: Path, args: list[str]) -> dict:
    r = subprocess.run([str(tsaur)] + args + ["--json"], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"{' '.join(args[:2])} failed ({r.returncode}): {r.stderr.strip() or r.stdout.strip()}")
    return json.loads(r.stdout)


def mib(n: int | None) -> str:
    return "n/a" if n is None else f"{n / (1 << 20):.1f}"


# ------------------------------------------------------------------ corpora


def generate_large(path: Path) -> None:
    """200 MB of pseudo-text (about 25 % compressible) followed by 60 MB of noise, deterministic."""
    if path.exists() and path.stat().st_size == 260 * (1 << 20):
        return
    rng = random.Random(20260924)
    vocab = ["".join(rng.choice("abcdefghijklmnopqrstuvwxyz") for _ in range(rng.randint(3, 9))) for _ in range(2000)]
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        written = 0
        target_text = 200 * (1 << 20)
        while written < target_text:
            words = rng.choices(vocab, k=180_000)
            lines = []
            for i in range(0, len(words), 12):
                lines.append(" ".join(words[i : i + 12]) + ".")
            chunk = ("\n".join(lines) + "\n").encode("ascii")
            chunk = chunk[: target_text - written]
            f.write(chunk)
            written += len(chunk)
        for _ in range(60):
            f.write(rng.randbytes(1 << 20))


def corpus_inputs(name: str, work_a: Path) -> list[Path]:
    if name == "S":
        return [BENCH / "corpus"]
    if name == "M":
        dirs = [BENCH / "corpus_versions"]
        if (BENCH / "corpus_real").is_dir():
            dirs.append(BENCH / "corpus_real")
        return dirs
    if name == "L":
        gen = work_a / "gen" / "L"
        generate_large(gen / "large.bin")
        return [gen]
    raise SystemExit(f"unknown corpus {name}")


def input_bytes(paths: list[Path]) -> int:
    total = 0
    for p in paths:
        for f in p.rglob("*"):
            if f.is_file():
                total += f.stat().st_size
    return total


# ------------------------------------------------------------------ roles


def cmd_prepare(a: argparse.Namespace) -> None:
    tsaur = Path(a.tsaur)
    if not tsaur.exists():
        raise SystemExit(f"build the release CLI first: cd tsaur && cargo build --release ({tsaur} missing)")
    work_a = Path(a.work) / "A"
    work_a.mkdir(parents=True, exist_ok=True)
    manifest = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "commit": git_commit(),
        "tsaur_version": tsaur_version(tsaur),
        "host_a": platform.platform(),
        "geometry": {"data": a.data, "parity": a.parity, "piece_size_kib": a.piece_size_kib},
        "corpora": [],
    }
    for name in a.corpora.split(","):
        name = name.strip()
        inputs = corpus_inputs(name, work_a)
        for p in inputs:
            if not p.exists():
                raise SystemExit(f"corpus {name}: {p} is missing (see benchmarks/README.md)")
        archive = work_a / f"{name}.tsr"
        t0 = time.perf_counter()
        pack = run_json(tsaur, ["pack", str(archive)] + [str(p) for p in inputs])
        t_pack = time.perf_counter() - t0
        vol_dir = work_a / name
        shutil.rmtree(vol_dir, ignore_errors=True)
        vol_dir.mkdir(parents=True)
        split_args = ["volumes", "split", str(archive), "--data", str(a.data), "--parity", str(a.parity), "--out", str(vol_dir)]
        if a.piece_size_kib:
            split_args += ["--piece-size-kib", str(a.piece_size_kib)]
        t0 = time.perf_counter()
        split = run_json(tsaur, split_args)
        t_split = time.perf_counter() - t0
        entry = {
            "name": name,
            "description": CORPORA.get(name, ""),
            "inputs": [str(p) for p in inputs],
            "input_bytes": input_bytes(inputs),
            "archive": str(archive),
            "archive_bytes": archive.stat().st_size,
            "archive_sha256": sha256_file(archive),
            "pack_seconds": round(t_pack, 3),
            "split_seconds": round(t_split, 3),
            "set_id": split["set_id"],
            "descriptor_b3": split["descriptor_b3"],
            "archive_b3": split["archive_b3"],
            "piece_size": split["piece_size"],
            "pieces": split["pieces"],
            "stripes": split["stripes"],
            "data": split["data"],
            "parity": split["parity"],
            "volume_bytes": sum(v["bytes"] for v in split["volumes"]) if split.get("volumes") and "bytes" in split["volumes"][0] else None,
            "volumes_dir": str(vol_dir),
        }
        manifest["corpora"].append(entry)
        print(f"{name}: {entry['input_bytes']} B input -> {entry['archive_bytes']} B archive ({pack.get('entries', '?')} entries), "
              f"{split['pieces']} pieces of {split['piece_size']} B, set {split['set_id'][:16]}")
    key = work_a / "server.key"
    if key.exists():
        fp = run_json(tsaur, ["volumes", "fingerprint", str(key)])["fingerprint"]
    else:
        fp = run_json(tsaur, ["volumes", "keygen", "--out", str(key)])["fingerprint"]
    manifest["server_fingerprint"] = fp
    out = work_a / "manifest.json"
    out.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(f"\nmanifest: {out}\nA's fingerprint: {fp}\ncopy the manifest to device B by hand (trusted channel), then run `serve` here and `fetch` on B")


def cmd_serve(a: argparse.Namespace) -> None:
    tsaur = Path(a.tsaur)
    work_a = Path(a.work) / "A"
    manifest = json.loads((work_a / "manifest.json").read_text(encoding="utf-8"))
    dirs = [c["volumes_dir"] for c in manifest["corpora"]]
    cmd = [str(tsaur), "volumes", "serve"] + dirs + ["--listen", a.listen]
    if not is_loopback(a.listen):
        cmd.append("--expose-lan")
    if a.plain:
        cmd.append("--allow-anyone")
    else:
        cmd += ["--tls-identity", str(work_a / "server.key")]
        if a.allow:
            for fp in a.allow:
                cmd += ["--allow", fp]
        else:
            cmd.append("--allow-anyone")
    if a.max_requests_per_second:
        cmd += ["--max-requests-per-second", str(a.max_requests_per_second)]
    print("running:", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd)
    sampler = Sampler(proc)
    t0 = time.perf_counter()
    try:
        while proc.poll() is None:
            if a.stop_file and Path(a.stop_file).exists():
                proc.terminate()
                break
            time.sleep(0.2)
        proc.wait(timeout=5)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    peak = sampler.finish()
    rec = {"listen": a.listen, "plain": a.plain, "seconds": round(time.perf_counter() - t0, 1), "peak_rss_bytes": peak, "host": platform.platform()}
    (work_a / "serve-peak.json").write_text(json.dumps(rec, indent=2), encoding="utf-8")
    print(f"\nserve ran {rec['seconds']} s, peak RSS {mib(peak)} MiB -> {work_a / 'serve-peak.json'}")


def cmd_keygen(a: argparse.Namespace) -> None:
    tsaur = Path(a.tsaur)
    work_b = Path(a.work) / "B"
    work_b.mkdir(parents=True, exist_ok=True)
    key = work_b / "client.key"
    if key.exists():
        fp = run_json(tsaur, ["volumes", "fingerprint", str(key)])["fingerprint"]
    else:
        fp = run_json(tsaur, ["volumes", "keygen", "--out", str(key)])["fingerprint"]
    print(f"B's identity: {key}\nB's fingerprint (give it to A for --allow): {fp}")


def fetch_once(tsaur: Path, c: dict, a: argparse.Namespace, out_dir: Path, volumes: str, join: Path | None, stop_after: int | None) -> dict:
    cmd = [str(tsaur), "volumes", "fetch", "--set", c["set_id"], "--descriptor", c["descriptor_b3"], "--from", a.from_addr, "--out", str(out_dir), "--volumes", volumes, "--json"]
    if not a.plain:
        cmd += ["--peer-id", a.peer_id, "--tls-identity", str(Path(a.work) / "B" / "client.key")]
    if join is not None:
        cmd += ["--join", str(join)]
    if stop_after is not None:
        cmd += ["--stop-after", str(stop_after)]
    code, out, err, wall, peak = run_measured(cmd)
    try:
        rep = json.loads(out)
    except json.JSONDecodeError:
        raise SystemExit(f"fetch failed ({code}): {err.strip() or out.strip()}")
    row = {
        "exit": code,
        "wall_seconds": round(wall, 3),
        "peak_rss_bytes": peak,
        "requests": rep.get("requests"),
        "bytes_received": rep.get("bytes_received"),
        "pieces_received": rep.get("pieces_received"),
        "pieces_reverified": rep.get("pieces_reverified"),
        "pieces_rejected": rep.get("pieces_rejected"),
        "seconds_connect": rep.get("seconds_connect"),
        "seconds_handshake": rep.get("seconds_handshake"),
        "seconds_transfer": rep.get("seconds_transfer"),
        "seconds_total": rep.get("seconds_total"),
        "seconds_join": rep.get("seconds_join"),
        "stopped_early": rep.get("stopped_early"),
        "reconstructible": rep.get("reconstructible"),
        "encrypted": rep.get("encrypted"),
        "archive_hash_ok": (rep.get("joined") or {}).get("archive_hash_ok"),
        "stderr": err.strip()[-400:],
    }
    if join is not None and join.exists():
        row["joined_sha256"] = sha256_file(join)
        row["sha256_matches_manifest"] = row["joined_sha256"] == c["archive_sha256"]
    return row


def cmd_fetch(a: argparse.Namespace) -> None:
    tsaur = Path(a.tsaur)
    if not tsaur.exists():
        raise SystemExit(f"build the release CLI first: cd tsaur && cargo build --release ({tsaur} missing)")
    if is_loopback(a.from_addr) and not a.same_machine_check:
        raise SystemExit("--from is a loopback address: both roles run on this machine. That is a tooling check, not a two-device "
                         "measurement; pass --same-machine-check to run it labelled as such.")
    if not a.plain and not a.peer_id:
        raise SystemExit("--peer-id <A's fingerprint from the manifest> is required (or --plain when A serves plain)")
    manifest = json.loads(Path(a.manifest).read_text(encoding="utf-8"))
    if not a.plain and a.peer_id.lower().replace(":", "") != manifest["server_fingerprint"].lower():
        print("note: --peer-id differs from the manifest's server fingerprint; the value you typed is the one that is pinned", file=sys.stderr)
    work_b = Path(a.work) / "B"
    work_b.mkdir(parents=True, exist_ok=True)
    wanted = [c.strip() for c in a.corpora.split(",")] if a.corpora else [c["name"] for c in manifest["corpora"]]
    results = {
        "status": "SINGLE-MACHINE TOOLING CHECK (not a measurement)" if a.same_machine_check else "two devices",
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "commit_b": git_commit(),
        "tsaur_version_b": tsaur_version(tsaur),
        "host_b": platform.platform(),
        "device_a": a.device_a,
        "device_b": a.device_b,
        "link": a.link,
        "raw_mbps": a.raw_mbps,
        "transport": "plain" if a.plain else "tls (pinned)",
        "from": a.from_addr,
        "repeats": a.repeats,
        "manifest": manifest,
        "runs": [],
        "resume": [],
    }
    for c in manifest["corpora"]:
        if c["name"] not in wanted:
            continue
        for volumes in ("needed", "all"):
            for r in range(a.repeats):
                out_dir = work_b / c["name"] / volumes
                shutil.rmtree(out_dir, ignore_errors=True)
                join = work_b / f"{c['name']}-{volumes}.tsr"
                if join.exists():
                    join.unlink()
                row = fetch_once(tsaur, c, a, out_dir, volumes, join, None)
                row.update({"corpus": c["name"], "volumes": volumes, "repeat": r + 1})
                results["runs"].append(row)
                print(f"{c['name']} {volumes} #{r + 1}: {row['bytes_received']} B in {row['seconds_total']} s "
                      f"(connect {row['seconds_connect']}, handshake {row['seconds_handshake']}, transfer {row['seconds_transfer']}, join {row['seconds_join']}), "
                      f"peak {mib(row['peak_rss_bytes'])} MiB, hash ok {row.get('sha256_matches_manifest')}", flush=True)
        if not a.skip_resume:
            base = [x["seconds_total"] for x in results["runs"] if x["corpus"] == c["name"] and x["volumes"] == "needed" and x["seconds_total"]]
            base_median = statistics.median(base) if base else None
            for frac in (0.25, 0.5, 0.75):
                k = max(1, round(frac * c["pieces"]))
                out_dir = work_b / c["name"] / f"resume-{int(frac * 100)}"
                shutil.rmtree(out_dir, ignore_errors=True)
                first = fetch_once(tsaur, c, a, out_dir, "needed", None, k)
                join = work_b / f"{c['name']}-resume-{int(frac * 100)}.tsr"
                if join.exists():
                    join.unlink()
                second = fetch_once(tsaur, c, a, out_dir, "needed", join, None)
                rec = {
                    "corpus": c["name"],
                    "fraction": frac,
                    "stop_after": k,
                    "first": first,
                    "resume": second,
                    "extra_seconds_vs_uninterrupted": (round(first["seconds_total"] + second["seconds_total"] - base_median, 3) if base_median else None),
                }
                results["resume"].append(rec)
                print(f"{c['name']} resume {int(frac * 100)} %: stopped after {k}, re-verified {second['pieces_reverified']}, "
                      f"re-fetched {second['pieces_received']}, extra {rec['extra_seconds_vs_uninterrupted']} s, hash ok {second.get('sha256_matches_manifest')}", flush=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out_json = work_b / f"results-{stamp}.json"
    out_json.write_text(json.dumps(results, indent=2), encoding="utf-8")
    out_md = work_b / "RESULTS-two-devices.md"
    out_md.write_text(render(results, out_json), encoding="utf-8")
    print(f"\nresults: {out_json}\nform:    {out_md}")


# ------------------------------------------------------------------ rendering


def render(res: dict, out_json: Path) -> str:
    m = res["manifest"]
    lines = ["# Two-device transfer measurements — results", "", f"**Status: {res['status']}**", ""]
    lines += ["## 1. Setup", "", "| Field | Device A (serves) | Device B (fetches) |", "|---|---|---|",
              f"| machine | {res['device_a'] or ''} | {res['device_b'] or ''} |",
              f"| OS | {m.get('host_a', '')} | {res['host_b']} |",
              f"| tsaur version, commit | {m.get('tsaur_version', '')}, {m.get('commit', '')} | {res['tsaur_version_b']}, {res['commit_b']} |",
              f"| address | {res['from']} | – |", "",
              "| Link | Value |", "|---|---|",
              f"| type | {res['link'] or ''} |", f"| raw throughput reference (Mbit/s) | {res['raw_mbps'] or ''} |",
              f"| transport | {res['transport']} |", f"| repeats | {res['repeats']} |", ""]
    lines += ["## 2. Corpora", "", "| Corpus | Input bytes | Archive bytes | Archive SHA-256 | Set id | Descriptor BLAKE3 | Piece size | Pieces | Geometry |", "|---|---:|---:|---|---|---|---:|---:|---|"]
    for c in m["corpora"]:
        lines.append(f"| {c['name']} | {c['input_bytes']} | {c['archive_bytes']} | `{c['archive_sha256'][:16]}…` | `{c['set_id'][:16]}…` | `{c['descriptor_b3'][:16]}…` | {c['piece_size']} | {c['pieces']} | {c['data']}+{c['parity']} |")
    lines += ["", "## 3. Fetch of the needed volumes, and of all volumes", "",
              "| Corpus | Volumes | Repeat | Requests | Bytes received | Connect s | TLS handshake s | Transfer s | Total s | Join s | MB/s | Pieces/s | Peak RSS MiB | Hash OK |",
              "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"]
    for r in res["runs"]:
        mbps = (r["bytes_received"] or 0) / 1e6 / r["seconds_total"] if r.get("seconds_total") else 0
        pps = (r["pieces_received"] or 0) / r["seconds_total"] if r.get("seconds_total") else 0
        lines.append(f"| {r['corpus']} | {r['volumes']} | {r['repeat']} | {r['requests']} | {r['bytes_received']} | {r['seconds_connect']:.3f} | {r['seconds_handshake']:.3f} | "
                     f"{r['seconds_transfer']:.3f} | {r['seconds_total']:.3f} | {r['seconds_join'] if r['seconds_join'] is not None else ''} | {mbps:.1f} | {pps:.0f} | {mib(r['peak_rss_bytes'])} | {r.get('sha256_matches_manifest')} |")
    lines += ["", "## 4. Interruption and resume", "",
              "| Corpus | Fraction | k | First run s | Resume: re-verified | Resume: re-fetched | Resume s | Extra s vs uninterrupted | Hash OK |",
              "|---|---:|---:|---:|---:|---:|---:|---:|---|"]
    for r in res["resume"]:
        lines.append(f"| {r['corpus']} | {int(r['fraction'] * 100)} % | {r['stop_after']} | {r['first']['seconds_total']:.3f} | {r['resume']['pieces_reverified']} | "
                     f"{r['resume']['pieces_received']} | {r['resume']['seconds_total']:.3f} | {r['extra_seconds_vs_uninterrupted']} | {r['resume'].get('sha256_matches_manifest')} |")
    lines += ["", "## 5. Peak memory on A (serving)", "", "Fill from `A/serve-peak.json` after stopping `serve` on device A.", "",
              "## 6. Notes and anomalies", "", f"Raw data: `{out_json.name}`. Non-empty stderr of any run is kept in the JSON (`stderr`).", ""]
    return "\n".join(lines)


# ------------------------------------------------------------------ main


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--tsaur", default=str(DEFAULT_TSAUR), help="path of the tsaur CLI (default: the release build)")
    sub = p.add_subparsers(dest="role", required=True)

    s = sub.add_parser("prepare", help="device A: corpora, archives, volumes, identity, manifest")
    s.add_argument("--work", required=True)
    s.add_argument("--corpora", default="S,M,L")
    s.add_argument("--data", type=int, default=4)
    s.add_argument("--parity", type=int, default=2)
    s.add_argument("--piece-size-kib", type=int, default=None)
    s.set_defaults(func=cmd_prepare)

    s = sub.add_parser("serve", help="device A: serve the volumes (Ctrl-C stops)")
    s.add_argument("--work", required=True)
    s.add_argument("--listen", required=True, help="address:port; a non-loopback address is exposed explicitly")
    s.add_argument("--allow", action="append", help="B's fingerprint (repeatable); without it anyone may fetch")
    s.add_argument("--plain", action="store_true", help="unencrypted mode (anyone may fetch)")
    s.add_argument("--max-requests-per-second", type=int, default=None)
    s.add_argument("--stop-file", default=None, help="stop serving when this file appears (for scripted runs)")
    s.set_defaults(func=cmd_serve)

    s = sub.add_parser("keygen", help="device B: identity for the encrypted mode")
    s.add_argument("--work", required=True)
    s.set_defaults(func=cmd_keygen)

    s = sub.add_parser("fetch", help="device B: run the measurements")
    s.add_argument("--work", required=True)
    s.add_argument("--manifest", required=True, help="A/manifest.json copied from device A")
    s.add_argument("--from", dest="from_addr", required=True)
    s.add_argument("--peer-id", default=None, help="A's fingerprint (from the manifest, checked by hand)")
    s.add_argument("--plain", action="store_true")
    s.add_argument("--repeats", type=int, default=3)
    s.add_argument("--corpora", default=None, help="subset, e.g. S,L")
    s.add_argument("--skip-resume", action="store_true")
    s.add_argument("--device-a", default="")
    s.add_argument("--device-b", default="")
    s.add_argument("--link", default="")
    s.add_argument("--raw-mbps", default="")
    s.add_argument("--same-machine-check", action="store_true", help="both roles on this machine: label the run as a tooling check")
    s.set_defaults(func=cmd_fetch)

    a = p.parse_args()
    a.func(a)


if __name__ == "__main__":
    main()
