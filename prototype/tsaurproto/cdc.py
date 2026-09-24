"""Content-Defined Chunking in the style of FastCDC (Xia et al., USENIX ATC 2016).

64-bit gear hash, two masks (level-2 normalized chunking): below the average size
we require a stricter mask, above it a more permissive one -> a more uniform chunk size distribution.
Pure Python implementation (slow, ~1-3 MB/s) - sufficient for the prototype.
"""
from __future__ import annotations
import random

_rng = random.Random(0x41495821)  # deterministic table (fixed seed)
GEAR = [_rng.getrandbits(64) for _ in range(256)]
MASK64 = (1 << 64) - 1


def _mask(bits: int) -> int:
    """Mask with `bits` bits set in the upper part of the hash."""
    return (((1 << bits) - 1) << (64 - bits)) & MASK64


def chunk_boundaries(data: bytes, min_size: int = 2048, avg_size: int = 8192, max_size: int = 65536) -> list[int]:
    """Returns the cut indices (exclusive); the last one == len(data)."""
    n = len(data)
    if n == 0:
        return []
    bits = max(1, avg_size.bit_length() - 1)
    mask_s = _mask(bits + 1)   # strict (below average)
    mask_l = _mask(bits - 1)   # permissive (above average)
    cuts: list[int] = []
    start = 0
    while start < n:
        end = min(n, start + max_size)
        if end - start <= min_size:
            cuts.append(end)
            break
        h = 0
        i = start + min_size
        limit_normal = min(end, start + avg_size)
        found = False
        while i < limit_normal:
            h = ((h << 1) + GEAR[data[i]]) & MASK64
            if not (h & mask_s):
                found = True
                break
            i += 1
        if not found:
            while i < end:
                h = ((h << 1) + GEAR[data[i]]) & MASK64
                if not (h & mask_l):
                    found = True
                    break
                i += 1
        cut = i + 1 if (found and i < end) else end
        cuts.append(cut)
        start = cut
    return cuts


def chunk(data: bytes, **kw) -> list[bytes]:
    out, prev = [], 0
    for c in chunk_boundaries(data, **kw):
        out.append(data[prev:c])
        prev = c
    return out
