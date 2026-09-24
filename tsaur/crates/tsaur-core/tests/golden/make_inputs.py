#!/usr/bin/env python3
"""Regenerate the golden-archive inputs (deterministic; the files are committed, this script documents them).

The inputs exercise every entry kind of format v1: plain text, an edited copy (dedup + delta),
incompressible bytes, a DOCX (ZIP/OPC container, deflate members -> preflate), a PDF with a
FlateDecode content stream and a DCTDecode image (Lepton in the full build), and a standalone
baseline JPEG. The JPEG is produced by the small encoder below (standard tables of ITU-T T.81
Annex K, one grey component) from a synthetic image, so nothing here is borrowed from anywhere.
"""

import math
from typing import Callable
import random
import zipfile
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "inputs"

# ITU-T T.81 Annex K: luminance quantisation table (K.1) and the standard Huffman tables (K.3, K.5).
QUANT = [16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99]
DC_BITS = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]
DC_VALS = list(range(12))
AC_BITS = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D]
AC_VALS = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
]
ZIGZAG = [0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63]


def huffman_codes(bits: list[int], vals: list[int]) -> dict[int, tuple[int, int]]:
    """Annex C: symbol -> (code, length)."""
    codes = {}
    code = 0
    k = 0
    for length in range(1, 17):
        for _ in range(bits[length - 1]):
            codes[vals[k]] = (code, length)
            code += 1
            k += 1
        code <<= 1
    return codes


class BitWriter:
    def __init__(self) -> None:
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def write(self, code: int, length: int) -> None:
        for i in range(length - 1, -1, -1):
            self.acc = (self.acc << 1) | ((code >> i) & 1)
            self.n += 1
            if self.n == 8:
                self.out.append(self.acc)
                if self.acc == 0xFF:
                    self.out.append(0)
                self.acc = 0
                self.n = 0

    def flush(self) -> bytes:
        while self.n:
            self.write(1, 1)  # pad with ones
        return bytes(self.out)


def magnitude(v: int) -> tuple[int, int]:
    """(category, additional bits) of a coefficient, T.81 F.1.2.1."""
    if v == 0:
        return 0, 0
    a = abs(v)
    cat = a.bit_length()
    bits = v if v > 0 else (v + (1 << cat) - 1)
    return cat, bits


def scale_quant(quality: int) -> list[int]:
    s = 5000 // quality if quality < 50 else 200 - 2 * quality
    return [max(1, min(255, (q * s + 50) // 100)) for q in QUANT]


def encode_jpeg_gray(width: int, height: int, pixel: Callable[[int, int], int], quality: int = 85) -> bytes:
    q = scale_quant(quality)
    dc = huffman_codes(DC_BITS, DC_VALS)
    ac = huffman_codes(AC_BITS, AC_VALS)
    cos = [[math.cos((2 * x + 1) * u * math.pi / 16) for x in range(8)] for u in range(8)]
    w = BitWriter()
    prev_dc = 0
    for by in range(0, height, 8):
        for bx in range(0, width, 8):
            block = [[pixel(min(bx + x, width - 1), min(by + y, height - 1)) - 128 for x in range(8)] for y in range(8)]
            coef = [0] * 64
            for v in range(8):
                for u in range(8):
                    s = 0.0
                    for y in range(8):
                        for x in range(8):
                            s += block[y][x] * cos[u][x] * cos[v][y]
                    cu = 1 / math.sqrt(2) if u == 0 else 1.0
                    cv = 1 / math.sqrt(2) if v == 0 else 1.0
                    coef[v * 8 + u] = int(round(0.25 * cu * cv * s / q[v * 8 + u]))
            zz = [coef[ZIGZAG[i]] for i in range(64)]
            diff = zz[0] - prev_dc
            prev_dc = zz[0]
            cat, bits = magnitude(diff)
            code, length = dc[cat]
            w.write(code, length)
            if cat:
                w.write(bits, cat)
            run = 0
            for i in range(1, 64):
                if zz[i] == 0:
                    run += 1
                    continue
                while run > 15:
                    code, length = ac[0xF0]
                    w.write(code, length)
                    run -= 16
                cat, bits = magnitude(zz[i])
                code, length = ac[(run << 4) | cat]
                w.write(code, length)
                w.write(bits, cat)
                run = 0
            if run:
                code, length = ac[0x00]
                w.write(code, length)
    scan = w.flush()

    def seg(marker: int, payload: bytes) -> bytes:
        return bytes([0xFF, marker]) + (len(payload) + 2).to_bytes(2, "big") + payload

    dqt = seg(0xDB, bytes([0]) + bytes(q[ZIGZAG[i]] for i in range(64)))
    sof = seg(0xC0, bytes([8]) + height.to_bytes(2, "big") + width.to_bytes(2, "big") + bytes([1, 1, 0x11, 0]))
    dht = seg(0xC4, bytes([0x00]) + bytes(DC_BITS) + bytes(DC_VALS) + bytes([0x10]) + bytes(AC_BITS) + bytes(AC_VALS))
    sos = seg(0xDA, bytes([1, 1, 0x00, 0, 63, 0]))
    return b"\xff\xd8" + dqt + sof + dht + sos + scan + b"\xff\xd9"


def synthetic_photo(width: int, height: int) -> Callable[[int, int], int]:
    """A smooth scene with texture: gradients, a soft disc, ripples and grain (deterministic)."""
    rng = random.Random(2026)
    grain = [[rng.randint(-6, 6) for _ in range(width)] for _ in range(height)]

    def pixel(x: int, y: int) -> int:
        cx, cy = width * 0.42, height * 0.55
        d = math.hypot(x - cx, y - cy)
        v = 90 + 60 * x / width + 30 * y / height
        v += 55 * math.exp(-(d / (width * 0.22)) ** 2)
        v += 18 * math.sin(x / 9.0) * math.cos(y / 13.0)
        v += 10 * math.sin((x + y) / 23.0)
        v += grain[y][x]
        return max(0, min(255, int(v)))

    return pixel


def text(seed: int, words: int) -> str:
    rng = random.Random(seed)
    vocab = ["archive", "agent", "chunk", "merkle", "tsaur", "verify", "content", "reference", "dictionary", "solid", "volume", "parity", "piece", "hash", "restore"]
    out = []
    for i in range(words):
        out.append(rng.choice(vocab))
        out.append("\n" if i % 11 == 10 else " ")
    return "".join(out)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "sub").mkdir(exist_ok=True)
    body = "# Notes on golden archives\n\n" + text(1, 6000)
    (OUT / "notes.md").write_text(body, encoding="utf-8", newline="\n")
    edited = body[:2000] + "INSERTED PARAGRAPH: this line exists only in the second version.\n" + body[2000:]
    (OUT / "sub" / "notes-v2.md").write_text(edited, encoding="utf-8", newline="\n")
    (OUT / "random.bin").write_bytes(random.Random(7).randbytes(4096))
    width, height = 192, 160
    photo = encode_jpeg_gray(width, height, synthetic_photo(width, height), quality=85)
    (OUT / "photo.jpg").write_bytes(photo)
    # minimal WordprocessingML package (deflated members)
    with zipfile.ZipFile(OUT / "report.docx", "w", compression=zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>')
        z.writestr(
            "word/document.xml",
            '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'
            '<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Golden report</w:t></w:r></w:p>'
            + "".join(f"<w:p><w:r><w:t>Paragraph {i}: {text(100 + i, 40).replace(chr(10), ' ')}</w:t></w:r></w:p>" for i in range(12))
            + "</w:body></w:document>",
        )
    # a small PDF: one FlateDecode content stream (with a font resource, so text extraction works)
    # and the same photo as a DCTDecode image object
    content = ("BT /F1 12 Tf 72 720 Td (" + text(5, 300).replace("\n", " ") + ") Tj ET\n").encode("ascii")
    flate = zlib.compress(content, 9)
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 5 0 R >> >> >>",
        b"<< /Length " + str(len(flate)).encode() + b" /Filter /FlateDecode >>\nstream\n" + flate + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width " + str(width).encode() + b" /Height " + str(height).encode()
        + b" /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Length " + str(len(photo)).encode() + b" >>\nstream\n" + photo + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    pdf = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, o in enumerate(objs, start=1):
        offsets.append(len(pdf))
        pdf += f"{i} 0 obj\n".encode() + o + b"\nendobj\n"
    xref = len(pdf)
    pdf += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        pdf += f"{off:010d} 00000 n \n".encode()
    pdf += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    (OUT / "paper.pdf").write_bytes(bytes(pdf))
    for p in sorted(OUT.rglob("*")):
        if p.is_file():
            print(f"{p.relative_to(OUT).as_posix():20} {p.stat().st_size:7} B")


if __name__ == "__main__":
    main()
