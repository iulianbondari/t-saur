"""Systematic Reed-Solomon over GF(2^8) (polynomial 0x11d).

Encoding matrix: Vandermonde brought to systematic form (identity on the first k rows),
decoding by inverting the sub-matrix corresponding to the available blocks (Gaussian elimination).
The data operations are vectorized with numpy (log/exp tables).
Purpose: demonstration of "recovery records" (PAR2/RAR-like): k data blocks + m parity
blocks; ANY k blocks out of the k+m rebuild the data.
"""
from __future__ import annotations
import numpy as np

EXP = np.zeros(512, dtype=np.uint8)
LOG = np.zeros(256, dtype=np.int32)
_x = 1
for _i in range(255):
    EXP[_i] = _x
    LOG[_x] = _i
    _x <<= 1
    if _x & 0x100:
        _x ^= 0x11D
for _i in range(255, 512):
    EXP[_i] = EXP[_i - 255]


def gf_mul(a: int, b: int) -> int:
    if a == 0 or b == 0:
        return 0
    return int(EXP[(LOG[a] + LOG[b]) % 255])


def gf_inv(a: int) -> int:
    return int(EXP[255 - LOG[a]])


def gf_mul_scalar_vec(a: int, v: np.ndarray) -> np.ndarray:
    if a == 0:
        return np.zeros_like(v)
    out = EXP[(LOG[a] + LOG[v]) % 255].astype(np.uint8)
    out[v == 0] = 0
    return out


def _gf_pow(a: int, e: int) -> int:
    r = 1
    for _ in range(e):
        r = gf_mul(r, a)
    return r


def _matmul(a, b):
    n, k, p = len(a), len(b), len(b[0])
    out = [[0] * p for _ in range(n)]
    for i in range(n):
        for j in range(p):
            s = 0
            for t in range(k):
                s ^= gf_mul(a[i][t], b[t][j])
            out[i][j] = s
    return out


def _invert(mat):
    n = len(mat)
    a = [row[:] + [1 if i == j else 0 for j in range(n)] for i, row in enumerate(mat)]
    for col in range(n):
        piv = next(r for r in range(col, n) if a[r][col] != 0)
        a[col], a[piv] = a[piv], a[col]
        inv = gf_inv(a[col][col])
        a[col] = [gf_mul(inv, x) for x in a[col]]
        for r in range(n):
            if r != col and a[r][col]:
                f = a[r][col]
                a[r] = [x ^ gf_mul(f, y) for x, y in zip(a[r], a[col])]
    return [row[n:] for row in a]


def encode_matrix(k: int, m: int) -> list[list[int]]:
    n = k + m
    vand = [[_gf_pow(r + 1, c) for c in range(k)] for r in range(n)]
    top = [row[:] for row in vand[:k]]
    return _matmul(vand, _invert(top))  # the first k rows become the identity


def encode(blocks: list[bytes], m: int) -> list[bytes]:
    """blocks: k blocks of equal length -> m parity blocks."""
    k = len(blocks)
    mat = encode_matrix(k, m)
    arrs = [np.frombuffer(b, dtype=np.uint8) for b in blocks]
    parity = []
    for r in range(k, k + m):
        acc = np.zeros(len(arrs[0]), dtype=np.uint8)
        for c in range(k):
            acc ^= gf_mul_scalar_vec(mat[r][c], arrs[c])
        parity.append(acc.tobytes())
    return parity


def decode(available: dict[int, bytes], k: int, m: int) -> list[bytes]:
    """available: index -> block (0..k-1 = data, k..k+m-1 = parity). Requires >= k blocks."""
    if len(available) < k:
        raise ValueError("not enough blocks for reconstruction")
    mat = encode_matrix(k, m)
    idx = sorted(available)[:k]
    inv = _invert([mat[i][:] for i in idx])
    arrs = [np.frombuffer(available[i], dtype=np.uint8) for i in idx]
    out = []
    for r in range(k):
        acc = np.zeros(len(arrs[0]), dtype=np.uint8)
        for c in range(k):
            acc ^= gf_mul_scalar_vec(inv[r][c], arrs[c])
        out.append(acc.tobytes())
    return out
