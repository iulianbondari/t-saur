//! Canonical views are derivatives: a document the converter cannot handle gets a note, never
//! blocks the original entry, and never takes the packer down (the third-party PDF converter
//! panics on some documents; that panic is contained).

use tsaur_core::{pack, PackOptions, Reader};

fn temp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tsaur-views-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A syntactically valid PDF whose page uses a font it never defines (a shape the converter
/// cannot process).
fn fontless_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 72 720 Td (text without a font resource) Tj ET\n";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(), content, b"\nendstream"].concat(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(o);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    pdf
}

#[test]
fn a_document_the_converter_cannot_handle_gets_a_note_and_the_original_stays_bit_exact() {
    let dir = temp("fontless");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let pdf = fontless_pdf();
    std::fs::write(src.join("odd.pdf"), &pdf).unwrap();
    std::fs::write(src.join("plain.txt"), b"a plain neighbour\n").unwrap();
    let out = dir.join("v.tsr");
    let rep = pack(std::slice::from_ref(&src), &out, PackOptions { canonical: true, ..Default::default() }).unwrap();
    assert_eq!(rep.entries, 2, "no view entry for the document that could not be converted");
    let mut r = Reader::open(&out, None).unwrap();
    let odd = r.entries().iter().find(|e| e.path == "odd.pdf").cloned().unwrap();
    assert!(odd.note.as_deref().is_some_and(|n| n.contains("canonical view not generated")), "{:?}", odd.note);
    assert!(r.verify(None).unwrap().entries_bad.is_empty());
    let restored = dir.join("out");
    r.extract(&restored, None, false).unwrap();
    assert_eq!(std::fs::read(restored.join("odd.pdf")).unwrap(), pdf);
    // on-demand conversion reports the failure as an error, not a crash
    let err = tsaur_core::canonical::pdf_to_text("odd.pdf", &pdf).unwrap_err().to_string();
    assert!(err.contains("converter failed") || err.contains("pdf text extraction"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
