"""Minimal CLI for the T-saur Python prototype (.tsrp archives).

Examples (from the repository root):
  python prototype/tsaur_proto.py pack   out.tsrp benchmarks/corpus/*  --solid --sidecars
  python prototype/tsaur_proto.py pack   out.tsrp benchmarks/corpus/*  --canonical --password secret
  python prototype/tsaur_proto.py list   out.tsrp
  python prototype/tsaur_proto.py unpack out.tsrp restored/ [--password secret]
  python prototype/tsaur_proto.py verify out.tsrp      # pieces + Merkle root (needs out.tsrp.meta)
  python prototype/tsaur_proto.py recover out.tsrp     # repairs corrupt pieces from out.tsrp.par
"""
from __future__ import annotations

import argparse
import glob
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from tsaurproto import archive as ax  # noqa: E402


def expand(patterns: list[str]) -> list[Path]:
    out: list[Path] = []
    for p in patterns:
        hits = [Path(h) for h in glob.glob(p)] or [Path(p)]
        out.extend(h for h in hits if h.is_file())
    return sorted(set(out))


def cmd_pack(a):
    files = expand(a.files)
    if not files:
        sys.exit("no files to archive")
    opts = ax.PackOptions(solid_block=(1 << 20) if a.solid else 0, canonical=a.canonical, password=a.password,
                          chunk_min=a.chunk // 4, chunk_avg=a.chunk, chunk_max=a.chunk * 8)
    stats = ax.pack(files, Path(a.out), opts)
    total = stats["input_bytes"]
    print(f"{len(files)} files, {total:,} bytes -> {stats['archive_bytes']:,} bytes "
          f"({100.0 * stats['archive_bytes'] / total:.1f}%), chunks {stats['chunks_total']} / unique {stats['chunks_unique']}, "
          f"codecs {stats['codec_hist']}, encrypted={stats['encrypted']}, {stats['pack_seconds']}s")
    if a.sidecars:
        meta = ax.write_sidecars(Path(a.out), piece_size=a.piece * 1024, parity_ratio=a.parity)
        print(f"sidecars: {meta['k']} pieces x {a.piece} KiB, {meta['m']} RS parity pieces, Merkle root {meta['root'].hex()}")


def cmd_list(a):
    r = ax.Reader(Path(a.archive), password=a.password)
    m = r.manifest
    print(f"TSRP v{m['v']}  chunking={m['chunking']}  solid_block={m['solid_block']}  canonical={m['canonical']}  "
          f"chunks={len(m['chunks'])}  blobs={len(m['blobs'])}  dict={'yes' if m['dict'] else 'no'}  crypto={'yes' if m['crypto'] else 'no'}")
    for e in m["entries"]:
        extra = e.get("note", "")
        print(f"  {e['size']:>10,}  {e['mode']:<12}  {e['h'].hex()[:16]}  {e['path']}  {extra}")


def cmd_unpack(a):
    r = ax.Reader(Path(a.archive), password=a.password)
    for p in r.extract_all(Path(a.outdir)):
        print("  ->", p)


def cmd_verify(a):
    bad, meta = ax.verify_pieces(Path(a.archive))
    print(f"pieces: {meta['k']} (+{meta['m']} parity), root {meta['root'].hex()}, corrupt: {bad or 'none'}")


def cmd_recover(a):
    fixed, ok = ax.recover(Path(a.archive))
    print(f"pieces repaired: {fixed}; final state: {'OK' if ok else 'FAILED (too many pieces missing)'}")


def main():
    p = argparse.ArgumentParser(description="T-saur prototype CLI (.tsrp archives)")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("pack"); s.add_argument("out"); s.add_argument("files", nargs="+")
    s.add_argument("--solid", action="store_true"); s.add_argument("--canonical", action="store_true")
    s.add_argument("--password"); s.add_argument("--sidecars", action="store_true")
    s.add_argument("--chunk", type=int, default=8192, help="average chunk size (bytes)")
    s.add_argument("--piece", type=int, default=64, help="P2P piece size (KiB)")
    s.add_argument("--parity", type=float, default=0.10, help="RS parity ratio (fraction of the data pieces)")
    s.set_defaults(fn=cmd_pack)
    s = sub.add_parser("list"); s.add_argument("archive"); s.add_argument("--password"); s.set_defaults(fn=cmd_list)
    s = sub.add_parser("unpack"); s.add_argument("archive"); s.add_argument("outdir"); s.add_argument("--password"); s.set_defaults(fn=cmd_unpack)
    s = sub.add_parser("verify"); s.add_argument("archive"); s.set_defaults(fn=cmd_verify)
    s = sub.add_parser("recover"); s.add_argument("archive"); s.set_defaults(fn=cmd_recover)
    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
