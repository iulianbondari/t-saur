//! Diagnostic: explode a ZIP container with the framing parser + preflate, rebuild it and report
//! whether the result is bit-exact, plus the per-member payload kinds and corrections sizes.

use std::env;
use tsaur_core::container;

fn main() {
    let path = env::args().nth(1).expect("usage: zipdiff <file.zip>");
    let data = std::fs::read(&path).expect("read");
    let Some((recipe, parts)) = container::explode(&data) else {
        println!("explode: framing not modelled (zip64, gaps, encryption) -> would be stored raw");
        return;
    };
    let mut plain_total = 0usize;
    let mut corr_total = 0usize;
    for (m, p) in recipe.members.iter().zip(parts.iter()) {
        let kind = match m.kind {
            0 => "stored",
            1 => "deflate/preflate",
            _ => "raw",
        };
        plain_total += p.len();
        corr_total += m.corrections.len();
        println!("  {:<40} {:<17} payload {:>8} B  corrections {:>5} B", m.name, kind, p.len(), m.corrections.len());
    }
    let out = container::rebuild(&recipe, &parts).expect("rebuild");
    let first_diff = data.iter().zip(out.iter()).position(|(a, b)| a != b);
    println!("original {} B, rebuilt {} B, bit-exact: {}, first diff: {:?}; chunked payload {} B, corrections {} B", data.len(), out.len(), out == data, first_diff, plain_total, corr_total);
}
