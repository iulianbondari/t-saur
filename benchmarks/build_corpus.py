"""
Builds the test corpus for T-saur:
  benchmarks/corpus/           -> 10 "random" files (.md, .txt, .docx, .pdf)
  benchmarks/corpus_versions/  -> the same 10 files + 5 edited versions (dedup scenario)

The text sources are open-source files already present on the machine (Python/PSF documentation,
CHANGELOGs of Rust crates (MIT/Apache), npm READMEs (MIT/ISC)). Nothing is downloaded.
The DOCX files are generated with python-docx, the PDFs with PyMuPDF (fitz).
"""
from __future__ import annotations
import hashlib, io, os, random, re, shutil, sys, textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CORPUS = ROOT / "corpus"
VERSIONS = ROOT / "corpus_versions"
HOME = Path.home()
CARGO = HOME / ".cargo/registry/src"
NODE = Path(r"C:\Program Files\nodejs\node_modules\npm\node_modules")

random.seed(20260922)

def find_one(base: Path, pattern: str) -> Path:
    hits = sorted(base.rglob(pattern))
    if not hits:
        raise SystemExit(f"cannot find {pattern} under {base}")
    return hits[0]

def pydoc_prose() -> str:
    from pydoc_data import topics
    parts = []
    for name, text in sorted(topics.topics.items()):
        parts.append(f"\n\n{name.upper()}\n{'=' * len(name)}\n\n{text}")
    return "".join(parts)

def wrap_lines(text: str, width: int = 95) -> list[str]:
    out = []
    for para in text.split("\n"):
        if not para.strip():
            out.append("")
        else:
            out.extend(textwrap.wrap(para, width=width, replace_whitespace=False, drop_whitespace=False) or [""])
    return out

def make_docx(path: Path, title: str, prose: str, table_rows: int = 12):
    import docx
    d = docx.Document()
    d.add_heading(title, level=0)
    d.add_paragraph("Synthetic document generated for the T-saur benchmark. The text comes from open-source documentation.")
    paras = [p for p in prose.split("\n\n") if p.strip()]
    for i, p in enumerate(paras):
        if i % 9 == 0:
            d.add_heading(p.strip().split("\n")[0][:70], level=1 if i % 27 == 0 else 2)
        else:
            d.add_paragraph(p.strip())
    t = d.add_table(rows=table_rows + 1, cols=4)
    t.style = "Table Grid"
    hdr = t.rows[0].cells
    for c, h in zip(hdr, ["Key", "Value", "Unit", "Notes"]):
        c.text = h
    for r in range(1, table_rows + 1):
        cells = t.rows[r].cells
        cells[0].text = f"item-{r:03d}"
        cells[1].text = f"{random.random()*1000:.3f}"
        cells[2].text = random.choice(["ms", "KB", "MB", "%", "ops/s"])
        cells[3].text = random.choice(["ok", "verified", "to review", "n/a"])
    d.save(path)

def make_pdf(path: Path, title: str, prose: str, with_image: bool = False, lines_per_page: int = 62):
    import fitz  # PyMuPDF
    doc = fitz.open()
    lines = [title, "=" * len(title), ""] + wrap_lines(prose)
    for start in range(0, len(lines), lines_per_page):
        page = doc.new_page(width=595, height=842)  # A4
        chunk = lines[start:start + lines_per_page]
        page.insert_text((50, 60), chunk, fontsize=8.5, fontname="helv", lineheight=1.35)
    if with_image:
        import numpy as np
        h, w = 480, 640
        yy, xx = np.mgrid[0:h, 0:w]
        base = np.stack([(xx * 255 // w), (yy * 255 // h), ((xx + yy) * 255 // (w + h))], axis=-1).astype(np.uint8)
        noise = np.random.default_rng(7).integers(0, 40, size=(h, w, 3), dtype=np.uint8)
        img = np.clip(base.astype(np.int16) + noise, 0, 255).astype(np.uint8)
        pix = fitz.Pixmap(fitz.csRGB, w, h, img.tobytes(), False)
        png = pix.tobytes("png")
        for pno in range(0, len(doc), 4):
            page = doc[pno]
            page.insert_image(fitz.Rect(320, 600, 560, 780), stream=png)
    doc.set_metadata({"title": title, "author": "T-saur benchmark", "creator": "PyMuPDF"})
    doc.save(path, garbage=4, deflate=True)
    doc.close()

def edit_text(text: str, n_edits: int = 12, tag: str = "v2") -> str:
    """Simulates a revision: a few lines inserted/deleted/modified, the rest identical."""
    lines = text.split("\n")
    rng = random.Random(hash(tag) & 0xffff)
    for _ in range(n_edits):
        i = rng.randrange(len(lines))
        op = rng.choice(["ins", "del", "mod"])
        if op == "ins":
            lines.insert(i, f"NOTE ({tag}): paragraph added at revision, line {i}.")
        elif op == "del" and len(lines) > 10:
            del lines[i]
        else:
            lines[i] = lines[i].replace("the", "THE", 1) + f" [rev {tag}]"
    return "\n".join(lines)

def sha(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()[:16]

def main():
    for d in (CORPUS, VERSIONS):
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)

    prose = pydoc_prose()
    print(f"pydoc prose: {len(prose):,} chars")
    third = len(prose) // 3

    src = {
        "changelog-node-gyp.md": find_one(NODE / "node-gyp", "CHANGELOG.md"),
        "readme-semver.md": find_one(NODE / "semver", "README.md"),
        "readme-encoding_rs.md": find_one(CARGO, "encoding_rs-*/README.md"),
        "python-license.txt": Path(sys.base_prefix) / "LICENSE.txt",
    }
    texts: dict[str, str] = {}
    for name, p in src.items():
        texts[name] = p.read_text(encoding="utf-8", errors="replace")
    texts["python-docs-part1.txt"] = prose[:third]
    news = Path(sys.base_prefix) / "Lib" / "idlelib" / "News3.txt"
    texts["idle-news.txt"] = news.read_text(encoding="utf-8", errors="replace") if news.exists() else prose[third:third + 60_000]

    for name, t in texts.items():
        (CORPUS / name).write_text(t, encoding="utf-8", newline="\n")

    make_docx(CORPUS / "report-A.docx", "Technical report A", prose[third:third + 70_000])
    make_docx(CORPUS / "report-B.docx", "Technical report B", texts["changelog-node-gyp.md"][:60_000], table_rows=30)
    make_pdf(CORPUS / "paper-A.pdf", "Paper A - documentation", prose[2 * third:2 * third + 120_000])
    make_pdf(CORPUS / "brochure-B.pdf", "Brochure B - with images", prose[2 * third + 120_000:2 * third + 160_000], with_image=True)

    # Versions scenario: copy the corpus + 5 edited files
    for p in CORPUS.iterdir():
        shutil.copy2(p, VERSIONS / p.name)
    (VERSIONS / "changelog-node-gyp.v2.md").write_text(edit_text(texts["changelog-node-gyp.md"], tag="md2"), encoding="utf-8", newline="\n")
    (VERSIONS / "python-docs-part1.v2.txt").write_text(edit_text(texts["python-docs-part1.txt"], 20, tag="txt2"), encoding="utf-8", newline="\n")
    (VERSIONS / "python-docs-part1.v3.txt").write_text(edit_text(edit_text(texts["python-docs-part1.txt"], 20, tag="txt2"), 15, tag="txt3"), encoding="utf-8", newline="\n")
    make_docx(VERSIONS / "report-A.v2.docx", "Technical report A (rev 2)", edit_text(prose[third:third + 70_000], 8, tag="docx2"))
    make_pdf(VERSIONS / "paper-A.v2.pdf", "Paper A - documentation (rev 2)", edit_text(prose[2 * third:2 * third + 120_000], 10, tag="pdf2"))

    for d in (CORPUS, VERSIONS):
        print(f"\n{d.name}:")
        total = 0
        for p in sorted(d.iterdir()):
            total += p.stat().st_size
            print(f"  {p.stat().st_size:>9,}  {sha(p)}  {p.name}")
        print(f"  {total:>9,}  TOTAL")

if __name__ == "__main__":
    main()
