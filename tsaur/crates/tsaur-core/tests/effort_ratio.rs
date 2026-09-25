//! Ratio gate of the codec-choice effort dial on the committed benchmark corpora (roadmap 1.1):
//! effort 3 must stay within 0.5 point of the input size of effort 5 and below `--codec zstd`.
//! Ignored by default (it packs the corpora three times each); run with `-- --ignored`. Times are
//! printed, not asserted (too noisy for CI; the benchmark protocol checks the time bound).

use std::path::PathBuf;
use tsaur_core::codec::CodecChoice;
use tsaur_core::{pack, PackOptions};

fn corpora() -> Option<Vec<PathBuf>> {
    let bench = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..").join("benchmarks");
    let dirs: Vec<PathBuf> = ["corpus", "corpus_versions"].iter().map(|n| bench.join(n)).collect();
    dirs.iter().all(|d| d.is_dir()).then_some(dirs)
}

#[test]
#[ignore = "packs the benchmark corpora; run with -- --ignored"]
fn effort_3_stays_within_half_a_point_of_the_full_trial_and_beats_zstd() {
    let Some(dirs) = corpora() else {
        println!("benchmarks/corpus and benchmarks/corpus_versions are not here (packaged crate?): skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("tsaur-effort-ratio-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    for dir in &dirs {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let mut size = std::collections::BTreeMap::new();
        let mut input = 0u64;
        for (label, opts) in
            [("zstd", PackOptions { codec: CodecChoice::Zstd, ..Default::default() }), ("effort 5", PackOptions::default()), ("effort 3", PackOptions { effort: 3, ..Default::default() })]
        {
            let archive = out.join(format!("{name}-{}.tsr", label.replace(' ', "")));
            let t = std::time::Instant::now();
            let rep = pack(std::slice::from_ref(dir), &archive, opts).unwrap();
            println!(
                "{name}: {label:<9} {:>10} B  {:5.2} %  {:.2} s wall  sampled {}  {:?}",
                rep.archive_bytes,
                100.0 * rep.archive_bytes as f64 / rep.input_bytes as f64,
                t.elapsed().as_secs_f64(),
                rep.blocks_sampled,
                rep.codec_hist
            );
            input = rep.input_bytes;
            size.insert(label, rep.archive_bytes);
        }
        let loss = (size["effort 3"] as f64 - size["effort 5"] as f64) / input as f64;
        assert!(loss <= 0.005, "{name}: effort 3 loses {:.2} points of the input against effort 5", 100.0 * loss);
        assert!(size["effort 3"] < size["zstd"], "{name}: effort 3 must beat --codec zstd: {size:?}");
    }
    let _ = std::fs::remove_dir_all(&out);
}
